"""Off-the-shelf segmentation models for AI masks (epic #160, model choice #170).

Nothing here is trained by us. `models.py` pins each upstream model (revision,
checksum, licence, training data) and its input/output contract; `export.py`
turns them into ONNX files plus a sample input/output pair for the Rust tests.
The weights are only ever used locally: they are never committed or shipped.
"""
