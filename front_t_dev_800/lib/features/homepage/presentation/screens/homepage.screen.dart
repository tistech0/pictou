import 'package:flutter/material.dart';
import 'package:front_t_dev_800/features/_global/presentation/widgets/app_bar.widget.dart';
import 'package:front_t_dev_800/features/homepage/presentation/widgets/triple_view_container.widget.dart';
import 'package:provider/provider.dart';

class HomePage extends StatefulWidget {
  const HomePage({super.key});

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  @override
  Widget build(BuildContext context) {
    return const Scaffold(
      appBar: CustomAppBar(),
      body: Padding(
        padding: EdgeInsets.all(8.0),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Padding(
              padding: EdgeInsets.only(bottom: 8.0),
              child: Text(
                'Album',
                style: TextStyle(
                  fontSize: 20,
                  fontWeight: FontWeight.bold,
                ),
              ),
            ),
            // Utilise TripleContainerWidget sans passer d'imageUrls
            TripleContainerWidget(),
            // Supprimé la seconde instance de TripleContainerWidget
            // car elle n'est plus nécessaire avec l'approche actuelle.
          ],
        ),
      ),
    );
  }
}
