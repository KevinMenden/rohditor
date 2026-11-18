"""Camera names must agree across sources (EXIF, the pixls table, CRW)."""

import pytest

from rohditor_ml.data.cameras import canonical_camera, exif_camera


@pytest.mark.parametrize(
    ("exif", "table"),
    [
        (("NIKON CORPORATION", "NIKON D40"), ("Nikon", "D40")),
        (("Canon", "Canon EOS 500D"), ("Canon", "EOS 500D")),
        (("Canon", "Canon EOS REBEL T4i"), ("Canon", "EOS Rebel T4i")),
        (("Panasonic", "DMC-GH1"), ("Panasonic", "DMC-GH1")),
        (("SONY", "ILCE-7RM3"), ("Sony", "ILCE-7RM3")),
        (("Canon\0", "Canon EOS D60\0"), ("Canon", "EOS D60")),
    ],
)
def test_exif_and_table_spellings_agree(exif, table):
    assert canonical_camera(*exif) == canonical_camera(*table)


def test_dng_converter_make_is_not_a_brand():
    assert canonical_camera("Adobe DNG Converter", "Canon EOS 5D Mark III") == "canon eos 5d mark iii"


def test_crw_make_and_model_come_from_the_ciff_heap(tmp_path):
    path = tmp_path / "shot.crw"
    path.write_bytes(b"II\x1a\x00\x00\x00HEAPCCDR" + b"\xff" * 100 + b"Canon\x00Canon EOS D60\x00" + b"\x00" * 50)
    assert exif_camera(path) == "canon eos d60"
