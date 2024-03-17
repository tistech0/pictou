use std::{error::Error as StdError, sync::Arc};

use actix_web::{
    dev::Payload,
    error::ErrorInternalServerError,
    http::header,
    post,
    web::{self, ServiceConfig},
    Error as ActixError, FromRequest, HttpRequest, HttpResponse, Result as ActixResult,
};
use diesel::{Queryable, Selectable};
use futures_util::future::LocalBoxFuture;
use jsonwebtoken as jwt;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    config::AppConfiguration,
    database::{self, Database},
    error::JsonHttpError,
};

use self::google::GoogleOAuth2Client;

pub mod google;
mod oauth2_client;

#[derive(Clone)]
pub struct Clients {
    google: Option<Arc<GoogleOAuth2Client>>,
}

impl Clients {
    #[tracing::instrument(skip_all)]
    pub async fn new(app_cfg: web::Data<AppConfiguration>) -> Self {
        let google = match GoogleOAuth2Client::new(app_cfg).await {
            Err(error) => {
                error!(%error, "failed to create Google OAuth2 client, Google login is now disabled!");
                None
            }
            Ok(google) => Some(Arc::new(google)),
        };
        Clients { google }
    }
}

/// Exposes all HTTP routes of this module.
pub fn routes(clients: &Clients, service_cfg: &mut ServiceConfig) {
    service_cfg.service(refresh_token);
    if let Some(google) = &clients.google {
        google::routes(google.clone(), service_cfg);
    }
    // cfg.configure(google::routes);
    // cfg.service(web::scope("/google").configure(google::routes));
}

const REFRESH_TOKEN_LIFETIME: Duration = Duration::days(30);
const ACCESS_TOKEN_LIFETIME: Duration = Duration::minutes(3);

/// Allows the user to request another access token after it expired.
/// This route checks the opaque refresh token against the database before granting (or not) the new access token.
#[post("/auth/refresh")]
#[tracing::instrument(skip_all)]
async fn refresh_token(
    db: web::Data<Database>,
    json: web::Json<RefreshTokenParams>,
    app_cfg: web::Data<AppConfiguration>,
) -> ActixResult<HttpResponse> {
    let RefreshTokenParams {
        user_id,
        refresh_token,
    } = json.into_inner();

    // check in DB that there is a user that satisfies the following conditions:
    // - user_id matches the one in the request
    // - refresh_token matches the one in the request
    // - refresh_token_exp is not expired
    let found_email: Option<String> = database::open(db.clone(), move |conn| {
        use crate::schema::users;
        use diesel::dsl::now;
        use diesel::prelude::*;

        Ok(users::table
            .filter(
                users::id.eq(user_id).and(
                    users::refresh_token
                        .eq(&refresh_token)
                        .and(users::refresh_token_exp.ge(now)),
                ),
            )
            .select(users::email)
            .first::<String>(conn)
            .optional()?)
    })
    .await?;

    let email = found_email.ok_or_else(|| {
        JsonHttpError::unauthorized("INVALID_CREDENTIALS", "invalid or expired credentials")
    })?;

    info!(%email, %user_id, lifetime_secs = ACCESS_TOKEN_LIFETIME.whole_seconds(), "refreshing access token of user");
    let access_token_exp = OffsetDateTime::now_utc() + ACCESS_TOKEN_LIFETIME;
    let access_token = AuthContext::encode_jwt(user_id, access_token_exp, &app_cfg.jwt_secret)?;

    // Regenerate a new refresh token and update the database
    let user_info = database::open(db, move |conn| {
        use crate::schema::users;
        use diesel::prelude::*;

        Ok(diesel::update(users::table.filter(users::id.eq(user_id)))
            .set((
                users::refresh_token.eq(&AuthContext::generate_refresh_token()),
                users::refresh_token_exp.eq(OffsetDateTime::now_utc() + REFRESH_TOKEN_LIFETIME),
            ))
            .returning(PersistedUserInfo::as_returning())
            .get_result(conn)?)
    })
    .await?;

    Ok(HttpResponse::Ok().json(AuthenticationResponse {
        user: user_info,
        access_token,
        access_token_exp,
    }))
}

#[derive(Debug, Deserialize)]
struct RefreshTokenParams {
    user_id: Uuid,
    /// Base64-encoded opaque binary token, preferably very long
    refresh_token: String,
}

#[derive(Debug, Serialize)]
struct AuthenticationResponse {
    #[serde(flatten)]
    user: PersistedUserInfo,
    access_token: String,
    access_token_exp: OffsetDateTime,
}

#[derive(Debug, Serialize, Selectable, Queryable)]
#[diesel(table_name = crate::schema::users)]
struct PersistedUserInfo {
    #[diesel(column_name = "id")]
    user_id: Uuid,
    email: String,
    refresh_token: Option<String>,
    refresh_token_exp: OffsetDateTime,
    name: Option<String>,
    given_name: Option<String>,
    family_name: Option<String>,
}

/// When present, this extractor ensures that the annotated route will require authentication.
/// Also provides the authenticated user's ID.
#[derive(Debug)]
#[non_exhaustive]
pub struct AuthContext {
    pub user_id: Uuid,
}

/// Allows [`AuthContext`] to be used as an Actix extractor.
impl FromRequest for AuthContext {
    type Error = ActixError;
    type Future = LocalBoxFuture<'static, ActixResult<Self>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        Box::pin(Self::authenticate_request(req.clone()))
    }
}

impl AuthContext {
    #[tracing::instrument]
    async fn authenticate_request(req: HttpRequest) -> ActixResult<Self> {
        let app_cfg = req
            .app_data::<web::Data<AppConfiguration>>()
            .ok_or_else(|| {
                error!("AppConfiguration not found in app data");
                ErrorInternalServerError("")
            })?;

        let token = Self::parse_bearer_token(&req)?;
        let auth_token = Self::decode_jwt(token, &app_cfg.jwt_secret)?;

        Ok(AuthContext {
            user_id: auth_token.claims.sub,
        })
    }

    /// Extracts the authorization header from the HTTP request and returns the bearer token if possible.
    fn parse_bearer_token(req: &HttpRequest) -> ActixResult<&str> {
        req.headers()
            .get(header::AUTHORIZATION)
            .and_then(|auth| auth.to_str().ok())
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .ok_or_else(|| {
                JsonHttpError::unauthorized(
                    "INVALID_AUTHORIZATION_HEADER",
                    "invalid or missing authorization header",
                )
            })
            .map_err(ActixError::from)
    }

    /// Attempts to decode a JWT token using the provided secret.
    fn decode_jwt(token: &str, secret: &[u8]) -> ActixResult<jwt::TokenData<AuthenticationToken>> {
        let key = jwt::DecodingKey::from_secret(secret);

        jwt::decode::<AuthenticationToken>(token, &key, &JWT_VALIDATION)
            .map_err(|_| JsonHttpError::unauthorized("INVALID_TOKEN", "malformed or invalid token"))
            .map_err(ActixError::from)
    }

    #[tracing::instrument(skip(secret))]
    fn encode_jwt(subject: Uuid, expires_at: OffsetDateTime, secret: &[u8]) -> ActixResult<String> {
        let header = jwt::Header::new(jwt::Algorithm::HS256);
        let key = jwt::EncodingKey::from_secret(secret);

        let claims = AuthenticationToken {
            aud: "pictou".to_owned(),
            iss: "pictou".to_owned(),
            exp: expires_at.unix_timestamp(),
            sub: subject,
        };

        jwt::encode(&header, &claims, &key).map_err(|error| {
            error!(error = &error as &dyn StdError, "failed to encode JWT");
            ErrorInternalServerError("")
        })
    }

    /// Generates a new random refresh token.
    fn generate_refresh_token() -> String {
        use base64::prelude::*;
        use rand::prelude::*;

        let mut rng = rand::thread_rng();
        let mut buf = [0u8; 128];
        rng.fill_bytes(&mut buf);

        BASE64_STANDARD.encode(buf)
    }
}

static JWT_VALIDATION: Lazy<jwt::Validation> = Lazy::new(|| {
    let mut validation = jwt::Validation::new(jwt::Algorithm::HS256);

    validation.validate_exp = true;
    validation.validate_aud = true;

    validation.set_audience(&["pictou"]);
    validation.set_issuer(&["pictou"]);

    validation
});

/// JWT claims
#[derive(Debug, Serialize, Deserialize)]
struct AuthenticationToken {
    aud: String,
    iss: String,
    exp: i64,
    sub: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{http::StatusCode, test::TestRequest, App, Responder};
    use tracing_test::traced_test;

    // {
    //   "sub": "f09d493d-a117-465d-84de-96fb4469dd40",
    //   "exp": 1916239022,
    //   "iss": "pictou",
    //   "aud": "pictou"
    // }
    const VALID_JWT: &str = "\
        eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJz\
        dWIiOiJmMDlkNDkzZC1hMTE3LTQ2NWQtODRkZS05N\
        mZiNDQ2OWRkNDAiLCJleHAiOjE5MTYyMzkwMjIsIm\
        lzcyI6InBpY3RvdSIsImF1ZCI6InBpY3RvdSJ9.7s\
        9spRPknrCpPLxLloVpUkPrmFxEyFklY7Bq-aF3d6I";

    // {
    //   "sub": "f09d493d-a117-465d-84de-96fb4469dd40",
    //   "exp": 1516239022,
    //   "iss": "pictou",
    //   "aud": "pictou"
    // }
    const EXPIRED_JWT: &str = "\
        eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJz\
        dWIiOiJmMDlkNDkzZC1hMTE3LTQ2NWQtODRkZS05N\
        mZiNDQ2OWRkNDAiLCJleHAiOjE1MTYyMzkwMjIsIm\
        lzcyI6InBpY3RvdSIsImF1ZCI6InBpY3RvdSJ9.zI\
        UtLivXOklBsSNbFcGgMS1uE9FSdd9E_qYm41c3gwQ";

    // {
    //   "sub": "f09d493d-a117-465d-84de-96fb4469dd40",
    //   "exp": 1916239022,
    //   "iss": "pictou",
    //   "aud": "picsou"
    // }
    const WRONG_AUD_JWT: &str = "\
        eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJz\
        dWIiOiJmMDlkNDkzZC1hMTE3LTQ2NWQtODRkZS05N\
        mZiNDQ2OWRkNDAiLCJleHAiOjE5MTYyMzkwMjIsIm\
        lzcyI6InBpY3RvdSIsImF1ZCI6InBpY3NvdSJ9.VX\
        PbzH1-JVM_-anGEMx7WK_u9_7GfGRTYwlnzARbAR8";

    #[test]
    fn test_parse_bearer_token() {
        assert!(AuthContext::parse_bearer_token(&TestRequest::get().to_http_request()).is_err());
        assert!(AuthContext::parse_bearer_token(
            &TestRequest::get()
                .insert_header(("Authorization", ""))
                .to_http_request()
        )
        .is_err());
        assert!(AuthContext::parse_bearer_token(
            &TestRequest::get()
                .insert_header(("Authorization", "Basic dGVzdA=="))
                .to_http_request()
        )
        .is_err());
        assert!(AuthContext::parse_bearer_token(
            &TestRequest::get()
                .insert_header(("Authorization", "Bearer"))
                .to_http_request()
        )
        .is_err());
        assert!(matches!(
            AuthContext::parse_bearer_token(
                &TestRequest::get()
                    .insert_header(("Authorization", "Bearer token"))
                    .to_http_request()
            ),
            Ok("token")
        ));
    }

    #[test]
    fn test_decode_jwt() {
        assert!(AuthContext::decode_jwt("invalid token", &[]).is_err());
        assert!(
            AuthContext::decode_jwt(EXPIRED_JWT, b"secret").is_err(),
            "expired token should fail"
        );
        assert!(
            AuthContext::decode_jwt(WRONG_AUD_JWT, b"secret").is_err(),
            "token with wrong audience should fail"
        );
        assert!(
            AuthContext::decode_jwt(VALID_JWT, b"wrong secret").is_err(),
            "invalid secret should fail"
        );

        let Ok(valid_token) = AuthContext::decode_jwt(VALID_JWT, b"secret") else {
            panic!("valid token failed to decode")
        };
        assert_eq!(
            valid_token.claims.sub,
            Uuid::parse_str("f09d493d-a117-465d-84de-96fb4469dd40").unwrap()
        );
    }

    #[test]
    fn test_encode_decode_jwt() {
        let secret = b"secret";

        let expires_at = OffsetDateTime::now_utc() + Duration::seconds(60);
        let encodded = AuthContext::encode_jwt(
            Uuid::parse_str("f09d493d-a117-465d-84de-96fb4469dd40").unwrap(),
            expires_at,
            secret,
        )
        .expect("failed to encode token");

        let decoded = AuthContext::decode_jwt(&encodded, secret).expect("failed to decode token");

        assert_eq!(decoded.header.alg, jwt::Algorithm::HS256);
        assert_eq!(
            decoded.claims.sub,
            Uuid::parse_str("f09d493d-a117-465d-84de-96fb4469dd40").unwrap()
        );
        assert_eq!(decoded.claims.aud, "pictou");
        assert_eq!(decoded.claims.iss, "pictou");
        assert_eq!(decoded.claims.exp, expires_at.unix_timestamp());
    }

    #[test]
    fn test_generate_refresh_token() {
        let token_a = AuthContext::generate_refresh_token();
        let token_b = AuthContext::generate_refresh_token();

        assert!(token_a.len() >= 128);
        assert!(token_b.len() >= 128);
        assert_ne!(token_a, token_b);
    }

    #[traced_test]
    #[actix_rt::test]
    async fn test_authenticate() {
        async fn authenticated_route(AuthContext { user_id }: AuthContext) -> impl Responder {
            HttpResponse::Ok().json(user_id)
        }

        let config = web::Data::new(AppConfiguration {
            jwt_secret: b"secret".as_ref().into(),
            ..Default::default()
        });

        let app = actix_web::test::init_service(
            App::new()
                .app_data(config.clone())
                .route("/authenticated", web::get().to(authenticated_route)),
        )
        .await;

        let no_auth_req = TestRequest::get().uri("/authenticated").to_request();
        let expired_token_req = TestRequest::get()
            .uri("/authenticated")
            .insert_header(("Authorization", format!("Bearer {EXPIRED_JWT}")))
            .to_request();
        let valid_token_req = TestRequest::get()
            .uri("/authenticated")
            .insert_header(("Authorization", format!("Bearer {VALID_JWT}")))
            .to_request();

        let no_auth_resp = actix_web::test::call_service(&app, no_auth_req).await;
        let expired_token_resp = actix_web::test::call_service(&app, expired_token_req).await;
        let valid_token_resp = actix_web::test::call_service(&app, valid_token_req).await;

        assert_eq!(no_auth_resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(expired_token_resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(valid_token_resp.status(), StatusCode::OK);
    }
}
