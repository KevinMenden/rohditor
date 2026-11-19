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


def tiff(order: str, make: bytes, model: bytes, ifd_at_end: bool) -> bytes:
    """A minimal TIFF with Make and Model in IFD0, as raw writers lay it out."""
    import struct

    strings = make + b"\0" + model + b"\0"
    ifd_size = 2 + 2 * 12 + 4
    strings_at, ifd_at = (8, 8 + len(strings)) if ifd_at_end else (8 + ifd_size, 8)
    header = (b"II" if order == "<" else b"MM") + struct.pack(order + "HI", 42, ifd_at)
    ifd = struct.pack(order + "H", 2)
    ifd += struct.pack(order + "HHII", 271, 2, len(make) + 1, strings_at)
    ifd += struct.pack(order + "HHII", 272, 2, len(model) + 1, strings_at + len(make) + 1)
    ifd += struct.pack(order + "I", 0)
    return header + (strings + ifd if ifd_at_end else ifd + strings)


@pytest.mark.parametrize("order", ["<", ">"])
@pytest.mark.parametrize("ifd_at_end", [False, True])
def test_tiff_make_model_in_either_byte_order_and_position(tmp_path, order, ifd_at_end):
    path = tmp_path / "shot.arw"
    path.write_bytes(tiff(order, b"SONY", b"ILCE-7C", ifd_at_end))
    assert exif_camera(path) == "sony ilce-7c"
