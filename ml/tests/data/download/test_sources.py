"""Listing/parsing logic of each source, on small synthetic upstream data."""

from rohditor_ml.data.download import licenses, pixls, rawnind
from rohditor_ml.data.download.files import RemoteFile
from rohditor_ml.data.download.selection import select_round_robin


def dataverse_entry(label, file_id, size=100):
    return {"label": label, "dataFile": {"id": file_id, "filesize": size, "md5": "ab" * 16}}


def test_rawnind_license_comes_from_permissions_or_dataset():
    permissions = rawnind.parse_permissions(
        "bananapi: CC0, by Benoit Brummer\n"
        "MuseeL-plaster: Copyrighted, unknown author, fair-use / research purpose only\n"
        "MuseeL-Hera: auntol: X, published under CC-BY license with permission\n"
    )
    assert permissions == {
        "bananapi": licenses.CC0,
        "MuseeL-plaster": licenses.RESEARCH_ONLY,
        "MuseeL-Hera": licenses.CC_BY,
    }
    sha = "1" * 40
    listing = {"data": {"files": [
        dataverse_entry(f"Bayer_bananapi_GT_ISO100_sha1={sha}.arw", 1),
        dataverse_entry(f"Bayer_bananapi_ISO6400_sha1={sha}.arw", 2),
        dataverse_entry(f"Bayer_unlisted_ISO100_sha1={sha}.cr2", 3),
        dataverse_entry(f"Bayer_MuseeL-plaster_ISO100_sha1={sha}.arw", 4),
        dataverse_entry(f"X-Trans_bananapi_ISO100_sha1={sha}.raf", 5),
        dataverse_entry(f"Bayer_TEST_Pen-pile_ISOHi 3200_sha1={sha}.nef", 6),
        dataverse_entry("permissions.txt", 7),
    ]}}
    scenes, skipped = rawnind.build_files(listing, permissions, {"Pen-pile"}, licenses.DEFAULT_ALLOWED)
    assert set(scenes) == {"bananapi"}
    clean = {f.name: f.meta["is_clean"] for f in scenes["bananapi"]}
    assert sorted(clean.values()) == [False, True]
    assert skipped["license CC-BY-SA-4.0"] == 2  # unlisted scene + Pen-pile (not in permissions)
    assert skipped["license LicenseRef-research-only"] == 1
    assert skipped["not a Bayer raw (X-Trans, sidecar or metadata)"] == 2


def test_rawnind_dataset_license_opt_in():
    sha = "1" * 40
    listing = {"data": {"files": [dataverse_entry(f"Bayer_unlisted_GT_ISO100_sha1={sha}.arw", 3)]}}
    allowed = licenses.DEFAULT_ALLOWED | {licenses.CC_BY_SA}
    scenes, _ = rawnind.build_files(listing, {}, set(), allowed)
    assert scenes["unlisted"][0].license == licenses.CC_BY_SA


def pixls_row(license_url="https://creativecommons.org/publicdomain/zero/1.0/", size="(10.55MB)", exif=True):
    sha = "a" * 64
    return [
        "Canon", "EOS 7D", "RAW (3:2)", 17.9, "",
        f"<a href='{license_url}' class='cc'>co</a>", "2016-12-29",
        f"<a href='https://raw.pixls.us/getfile.php/129/nice/Canon - EOS 7D.CR2'>x</a>"
        f"<div><span>{sha}</span>&nbsp;{size}</div>",
        "<a href='https://raw.pixls.us/getfile.php/129/exif/e.txt'>exifdata</a>" if exif else "",
    ]


def test_pixls_row_parsing():
    file = pixls.parse_row(pixls_row())
    assert file.license == licenses.CC0
    assert file.name == "129_Canon_-_EOS_7D.CR2"
    assert file.size == 10_550_000 and not file.exact_size
    assert file.checksum.hexdigest == "a" * 64
    assert pixls.parse_row(pixls_row(size="(823.56kB)")).size == 823_560
    assert pixls.parse_row(pixls_row(exif=False)).meta["exif_url"] is None
    nc = pixls.parse_row(pixls_row(license_url="http://creativecommons.org/licenses/by-nc-sa/4.0/"))
    assert nc.license == "CC-BY-NC-SA-4.0" and nc.license not in licenses.DEFAULT_ALLOWED


def test_pixls_iso():
    assert pixls.parse_iso("Exif.Photo.ISOSpeedRatings                    1250\n") == 1250
    assert pixls.parse_iso("Exif.Photo.ISOSpeedRatings   0\nExif.Photo.ISOSpeed 200\n") == 200
    assert pixls.parse_iso("Exif.Canon.ISOSpeed   Auto\n") is None


def test_round_robin_spreads_over_groups_and_respects_budget():
    def f(name, size):
        return RemoteFile("t", name, "u", size, "CC0-1.0")

    groups = {"a": [[f(f"a{i}", 10)] for i in range(5)], "b": [[f(f"b{i}", 10)] for i in range(5)]}
    chosen = select_round_robin(groups, 40)
    assert len(chosen) == 4
    assert sorted(x.name[0] for x in chosen) == ["a", "a", "b", "b"]
    assert chosen == select_round_robin(groups, 40)  # deterministic
    # A bundle stays together, and one that doesn't fit is skipped, not truncated.
    bundles = {"s": [[f("big1", 30), f("big2", 30)], [f("small", 5)]]}
    assert [x.name for x in select_round_robin(bundles, 20)] == ["small"]
