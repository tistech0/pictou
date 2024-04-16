// Openapi Generator last run: : 2024-04-03T10:24:05.763224
import 'package:openapi_generator_annotations/openapi_generator_annotations.dart';

@Openapi(
  additionalProperties: DioProperties(
      pubName: 'pictouapi', pubAuthor: 'Pictou', pubVersion: '0.0.1'),
  inputSpec: InputSpec(path: 'lib/api/openapi.json'),
  generatorName: Generator.dio,
  runSourceGenOnOutput: true,
  outputDirectory: 'api/',
)
class Example {}
