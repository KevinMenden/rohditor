# Known Open Issues


## Bugs/Issues
- Capture sharpening needs real image test: currently the setup is not really doing any sharpening when tested with real images
- White balance still behaves weirdly: changing the slider leads to changes that are too drastic. This needs to be checked
- HSL/three way grading always forces the entire pipeline on the CPU
- Gamut compression seems off. It clearly changes the colors in the preview image --> is this expected? Double-check this feature as well
- Verify that lens correction actually works as intented. 
- The Sony 70-350 lens is not detected during lens correction

## Bigger Architectural Issues
- GPU pipeline is to weak: we want to run as much as possible on the GPU. That is currently not the case. Needs to be addressed (e.g. demosaic on the GPU)
- Demosaic-algorithms need to be verified properly: the results especially from RCD need to be checked, as they appear of too low quality sometimes
  - compared to RawTherapee, Amaze seems to have more artifacts. We should create reference images that those algorithms should also achieve. However, there are of course other parts in the pipeline to consider for this
- Improve the default "Rohditor Standard" look - it's currently still too flat
- The pipeline is too slow: RawTherapee is much faster in e.g. applying a demosaic algorithm - maybe the entire pipeline is better structured
