"""Tests for picard_post_save.

Run: python3 -m unittest discover -s tools/picard -v

The FLAC-dependent tests need the `flac` and `ffmpeg` binaries and self-skip
without them. The rip check is what deletes audio, so those tests matter most:
do not let them silently skip everywhere.
"""

import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import picard_post_save as mod  # noqa: E402

HAVE_FLAC = shutil.which("flac") is not None
HAVE_FFMPEG = shutil.which("ffmpeg") is not None


def make_flac(path, seconds="0.2"):
    """Generate a small valid FLAC. Returns True on success."""
    result = subprocess.run(
        ["ffmpeg", "-f", "lavfi", "-i", f"sine=f=440:d={seconds}",
         "-c:a", "flac", path, "-y"],
        capture_output=True,
    )
    return result.returncode == 0 and os.path.isfile(path)


def corrupt_flac_payload(path):
    """Corrupt a FLAC's audio stream so `flac -t` fails, keeping the header."""
    with open(path, "r+b") as fh:
        fh.seek(0, os.SEEK_END)
        size = fh.tell()
        # Overwrite the back half: past the header, squarely in the stream.
        fh.seek(size // 2)
        fh.write(b"\xde\xad\xbe\xef" * ((size // 2) // 4))


# The command template previously configured in Picard.ini (pta_command). The
# plugin interpolates metadata into this string and only then runs shlex.split
# on it, so any quote in a value is re-parsed as a shell quote.
LEGACY_TEMPLATE = (
    'python3 /config/tools/picard_post_save.py -n 8 "%folderpath%" "%albumartist%" "%album%"'
)
FIXED_TEMPLATE = 'python3 /config/tools/picard_post_save.py -n 8 "%folderpath%"'


def interpolate(template, folderpath, albumartist, album):
    """Mimic post_tagging_actions: substitute variables, then shlex.split.

    The real plugin does `variables_pattern.sub('{}', command)` then
    `.format(*values)`; the substitution is modeled with str.replace here
    because what is under test is the *tokenize-after-substitute* ordering,
    which is identical either way.
    """
    command = (
        template.replace("%folderpath%", folderpath)
        .replace("%albumartist%", albumartist)
        .replace("%album%", album)
    )
    return shlex.split(command)


class TestPluginQuoteHandling(unittest.TestCase):
    """Characterize the upstream defect the Picard command has to avoid.

    These pin the *reason* metadata is no longer passed on the command line.
    They test the plugin's behavior, not this script's.
    """

    def test_legacy_template_mangles_balanced_quotes(self):
        argv = interpolate(LEGACY_TEMPLATE, "/music/A/B", "Artist", 'Say "Hello"')
        # The quotes are silently eaten: covit would get the wrong album title.
        self.assertEqual(argv[-1], "Say Hello")

    def test_legacy_template_dies_on_unbalanced_quote(self):
        with self.assertRaises(ValueError):
            interpolate(LEGACY_TEMPLATE, "/music/A/B", "Artist", 'The 12" Mixes')

    def test_fixed_template_passes_only_the_folder(self):
        # The fix is structural: no metadata placeholder is present to mangle.
        self.assertNotIn("%album", FIXED_TEMPLATE)
        self.assertNotIn("%albumartist%", FIXED_TEMPLATE)
        argv = interpolate(FIXED_TEMPLATE, "/music/A/B", "Artist", 'The 12" Mixes')
        self.assertEqual(argv[-1], "/music/A/B")

    def test_quote_in_the_folder_path_would_still_break(self):
        # Documents why windows_compatibility (quote -> underscore in paths) is
        # a hard prerequisite, not incidental.
        with self.assertRaises(ValueError):
            interpolate(FIXED_TEMPLATE, '/music/The 12" Mixes', "Artist", "Album")


class TestParseArgs(unittest.TestCase):
    def test_metadata_args_are_optional(self):
        args = mod.parse_args(["-n", "8", "/music/A/B"])
        self.assertEqual(args.folderpath, "/music/A/B")
        self.assertIsNone(args.albumartist)
        self.assertIsNone(args.album)

    def test_metadata_args_still_accepted(self):
        args = mod.parse_args(["/music/A/B", "Artist", "Album"])
        self.assertEqual(args.albumartist, "Artist")
        self.assertEqual(args.album, "Album")


class TestFindQueryFile(unittest.TestCase):
    def test_picks_first_audio_file_alphabetically(self):
        with tempfile.TemporaryDirectory() as tmp:
            for name in ["02_b.flac", "01_a.flac", "cover.jpg", "notes.txt"]:
                open(os.path.join(tmp, name), "w").close()
            self.assertEqual(
                mod.find_query_file(tmp), os.path.join(tmp, "01_a.flac")
            )

    def test_ignores_non_audio_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            for name in ["folder.jpg", "album.nfo", "CORRUPT_RIP.log"]:
                open(os.path.join(tmp, name), "w").close()
            self.assertIsNone(mod.find_query_file(tmp))

    def test_accepts_non_flac_audio(self):
        with tempfile.TemporaryDirectory() as tmp:
            open(os.path.join(tmp, "track.m4a"), "w").close()
            self.assertEqual(
                mod.find_query_file(tmp), os.path.join(tmp, "track.m4a")
            )

    def test_ignores_subdirectory_with_audio_extension(self):
        with tempfile.TemporaryDirectory() as tmp:
            os.mkdir(os.path.join(tmp, "weird.flac"))
            self.assertIsNone(mod.find_query_file(tmp))

    def test_missing_directory_returns_none(self):
        self.assertIsNone(mod.find_query_file("/nonexistent/path/xyz"))


class TestAppleDoubleSidecars(unittest.TestCase):
    """A Mac writing to an SMB share leaves a `._<name>` twin beside each file.

    It carries the same extension but is not audio, so `flac -t` calls it
    CONFIRMED CORRUPT -- which would delete the whole album -- and it sorts
    ahead of every digit, so it would also win the covit query-file pick.
    """

    def test_find_flacs_ignores_appledouble(self):
        with tempfile.TemporaryDirectory() as tmp:
            real = os.path.join(tmp, "01 - track.flac")
            open(real, "w").close()
            open(os.path.join(tmp, "._01 - track.flac"), "w").close()
            self.assertEqual(mod.find_flacs(tmp), [real])

    def test_find_query_file_skips_appledouble(self):
        with tempfile.TemporaryDirectory() as tmp:
            real = os.path.join(tmp, "01 - track.flac")
            open(os.path.join(tmp, "._01 - track.flac"), "w").close()
            open(real, "w").close()
            self.assertEqual(mod.find_query_file(tmp), real)

    def test_find_query_file_skips_dotfiles(self):
        with tempfile.TemporaryDirectory() as tmp:
            open(os.path.join(tmp, ".hidden.flac"), "w").close()
            self.assertIsNone(mod.find_query_file(tmp))

    def test_delete_audio_files_leaves_appledouble(self):
        with tempfile.TemporaryDirectory() as tmp:
            real = os.path.join(tmp, "01.flac")
            sidecar = os.path.join(tmp, "._01.flac")
            open(real, "w").close()
            open(sidecar, "w").close()

            deleted = mod.delete_audio_files(tmp)

            self.assertEqual(deleted, [real])
            self.assertTrue(os.path.isfile(sidecar))


@unittest.skipUnless(HAVE_FLAC and HAVE_FFMPEG, "needs flac and ffmpeg")
class TestAppleDoubleEndToEnd(unittest.TestCase):
    def test_appledouble_does_not_condemn_the_album(self):
        """The regression: a sidecar must not get the real album deleted."""
        with tempfile.TemporaryDirectory() as tmp:
            real = os.path.join(tmp, "01 - track.flac")
            self.assertTrue(make_flac(real))
            # A real AppleDouble header: valid file, definitely not a FLAC.
            with open(os.path.join(tmp, "._01 - track.flac"), "wb") as fh:
                fh.write(b"\x00\x05\x16\x07\x00\x02\x00\x00Mac OS X        ")

            result = subprocess.run(
                [sys.executable, os.path.join(HERE, "picard_post_save.py"), tmp],
                capture_output=True, text=True,
                env=dict(os.environ, COVIT_BIN="/nonexistent/covit"),
            )

            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue(os.path.isfile(real), "the real track was deleted")
            self.assertFalse(
                os.path.exists(os.path.join(tmp, mod.LOG_FILENAME)),
                "a tombstone was left that would condemn any re-rip",
            )


class TestRunCovit(unittest.TestCase):
    """run_covit must never destroy existing artwork it cannot replace."""

    def _run(self, tmp, covit_exists=True, returncode=0, **kwargs):
        """Run run_covit with subprocess stubbed. Returns the argv used."""
        captured = {}

        def fake_run(cmd, **_):
            captured["cmd"] = cmd
            return mock.Mock(returncode=returncode, stdout="", stderr="")

        covit_path = os.path.join(tmp, "covit-bin")
        if covit_exists:
            open(covit_path, "w").close()

        with mock.patch.object(mod, "COVIT_BIN", covit_path), \
                mock.patch.object(mod.subprocess, "run", side_effect=fake_run):
            result = mod.run_covit(tmp, **kwargs)
        return captured.get("cmd"), result

    def test_uses_input_file_so_covit_reads_tags_itself(self):
        with tempfile.TemporaryDirectory() as tmp:
            track = os.path.join(tmp, "01.flac")
            open(track, "w").close()
            cmd, ok = self._run(tmp)
        self.assertTrue(ok)
        self.assertIn("--input", cmd)
        self.assertEqual(cmd[cmd.index("--input") + 1], track)
        # Metadata must NOT be re-derived and passed as strings: that is what
        # flattened multi-value artist credits.
        self.assertNotIn("--query-artist", cmd)
        self.assertNotIn("--query-album", cmd)

    def test_falls_back_to_args_when_no_audio_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            cmd, ok = self._run(tmp, albumartist="Artist", album='The 12" Mixes')
        self.assertTrue(ok)
        self.assertNotIn("--input", cmd)
        # The quote survives: argv list, never a shell string.
        self.assertEqual(cmd[cmd.index("--query-album") + 1], 'The 12" Mixes')

    def test_input_file_preferred_over_supplied_args(self):
        with tempfile.TemporaryDirectory() as tmp:
            open(os.path.join(tmp, "01.flac"), "w").close()
            cmd, _ = self._run(tmp, albumartist="Only One Of Two", album="Album")
        self.assertIn("--input", cmd)
        self.assertNotIn("--query-artist", cmd)

    def test_skips_when_nothing_to_query_from(self):
        with tempfile.TemporaryDirectory() as tmp:
            cmd, ok = self._run(tmp)
        self.assertIsNone(cmd, "covit must not run with no query source")
        self.assertFalse(ok)

    def test_missing_covit_binary_preserves_existing_artwork(self):
        with tempfile.TemporaryDirectory() as tmp:
            art = os.path.join(tmp, "folder.jpg")
            open(art, "w").close()
            open(os.path.join(tmp, "01.flac"), "w").close()
            cmd, ok = self._run(tmp, covit_exists=False)
            # Assert INSIDE the context manager: the tempdir (and everything
            # in it) is gone by the time the with-block exits.
            self.assertIsNone(cmd)
            self.assertFalse(ok)
            self.assertTrue(
                os.path.isfile(art),
                "existing artwork was deleted even though covit could not run",
            )

    def test_no_query_source_preserves_existing_artwork(self):
        with tempfile.TemporaryDirectory() as tmp:
            art = os.path.join(tmp, "folder.jpg")
            open(art, "w").close()
            self._run(tmp)
            self.assertTrue(
                os.path.isfile(art),
                "existing artwork was deleted with no query source",
            )

    def test_existing_artwork_cleaned_when_covit_will_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            art = os.path.join(tmp, "folder.jpg")
            open(art, "w").close()
            open(os.path.join(tmp, "01.flac"), "w").close()
            self._run(tmp)
            self.assertFalse(
                os.path.isfile(art), "stale artwork should be cleared for covit"
            )


class TestCleanExistingArtwork(unittest.TestCase):
    def test_removes_covit_duplicates_and_keeps_unrelated_images(self):
        with tempfile.TemporaryDirectory() as tmp:
            targets = ["folder.jpg", "folder (1).jpg", "cover.png"]
            keepers = ["artist.jpg", "back.jpg", "booklet-03.jpg"]
            for name in targets + keepers:
                open(os.path.join(tmp, name), "w").close()

            removed = mod.clean_existing_artwork(tmp)

            self.assertEqual(len(removed), len(targets))
            for name in targets:
                self.assertFalse(os.path.exists(os.path.join(tmp, name)), name)
            for name in keepers:
                self.assertTrue(os.path.exists(os.path.join(tmp, name)), name)

    def test_leaves_audio_alone(self):
        with tempfile.TemporaryDirectory() as tmp:
            track = os.path.join(tmp, "folder.flac")
            open(track, "w").close()
            mod.clean_existing_artwork(tmp)
            self.assertTrue(os.path.isfile(track))


@unittest.skipUnless(HAVE_FLAC and HAVE_FFMPEG, "needs flac and ffmpeg")
class TestStreamTesting(unittest.TestCase):
    """The deletion trigger. A wrong answer here destroys a user's music."""

    def test_clean_flac_reports_ok(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "t.flac")
            self.assertTrue(make_flac(path))
            status, _ = mod.test_flac_stream(path)
        self.assertEqual(status, mod.StreamStatus.OK)

    def test_corrupt_flac_reports_corrupt(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "t.flac")
            self.assertTrue(make_flac(path))
            corrupt_flac_payload(path)
            status, detail = mod.test_flac_stream(path)
        self.assertEqual(status, mod.StreamStatus.CORRUPT)
        self.assertTrue(detail, "a corruption verdict must carry the flac error")

    def test_missing_binary_is_tool_error_not_corruption(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "t.flac")
            self.assertTrue(make_flac(path))
            with mock.patch.object(
                mod.subprocess, "run", side_effect=FileNotFoundError()
            ):
                status, _ = mod.test_flac_stream(path)
        # Critical distinction: TOOL_ERROR never deletes, CORRUPT does.
        self.assertEqual(status, mod.StreamStatus.TOOL_ERROR)

    def test_empty_file_is_tool_error_not_corruption(self):
        """An interrupted copy must not condemn the album it landed in."""
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "incomplete.flac")
            open(path, "w").close()
            status, detail = mod.test_flac_stream(path)
        self.assertEqual(status, mod.StreamStatus.TOOL_ERROR)
        self.assertIn("empty", detail)

    @unittest.skipIf(os.geteuid() == 0, "root bypasses file permissions")
    def test_unreadable_file_is_tool_error_not_corruption(self):
        """A uid mismatch on a share must not read as a bad rip."""
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "locked.flac")
            self.assertTrue(make_flac(path))
            os.chmod(path, 0o000)
            try:
                status, detail = mod.test_flac_stream(path)
            finally:
                os.chmod(path, 0o644)
        self.assertEqual(status, mod.StreamStatus.TOOL_ERROR)
        self.assertIn("not readable", detail)

    def test_signal_kill_is_tool_error_not_corruption(self):
        """An OOM-killed worker says nothing about the audio.

        subprocess reports a signal death as a negative returncode; with -n 8
        in a container a SIGKILL mid-run is routine and the files are fine.
        """
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "t.flac")
            self.assertTrue(make_flac(path))
            with mock.patch.object(
                mod.subprocess, "run",
                return_value=mock.Mock(returncode=-9, stdout="", stderr=""),
            ):
                status, detail = mod.test_flac_stream(path)
        self.assertEqual(status, mod.StreamStatus.TOOL_ERROR)
        self.assertIn("signal 9", detail)

    def test_nonzero_exit_without_a_reason_is_not_corruption(self):
        """A corruption verdict must carry flac's diagnosis, not just a code."""
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "t.flac")
            self.assertTrue(make_flac(path))
            with mock.patch.object(
                mod.subprocess, "run",
                return_value=mock.Mock(returncode=1, stdout="", stderr="   "),
            ):
                status, _ = mod.test_flac_stream(path)
        self.assertEqual(status, mod.StreamStatus.TOOL_ERROR)

    def test_real_corruption_still_carries_its_diagnosis(self):
        """Guard against the above turning the gate off."""
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "t.flac")
            self.assertTrue(make_flac(path))
            corrupt_flac_payload(path)
            status, detail = mod.test_flac_stream(path)
        self.assertEqual(status, mod.StreamStatus.CORRUPT)
        self.assertTrue(detail.strip(), "flac must report why it failed")

    def test_freshly_written_file_is_still_checked(self):
        """Picard rewrites tags, so a recent mtime must not skip the check.

        A "still being written" window would make the gate pass on every real
        run without ever looking at the audio.
        """
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "t.flac")
            self.assertTrue(make_flac(path))
            corrupt_flac_payload(path)
            os.utime(path, None)  # mtime = now, exactly as after a tag write
            status, _ = mod.test_flac_stream(path)
        self.assertEqual(status, mod.StreamStatus.CORRUPT)

    def test_timeout_is_tool_error_not_corruption(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "t.flac")
            self.assertTrue(make_flac(path))
            with mock.patch.object(
                mod.subprocess, "run",
                side_effect=subprocess.TimeoutExpired(cmd="flac", timeout=300),
            ):
                status, _ = mod.test_flac_stream(path)
        self.assertEqual(status, mod.StreamStatus.TOOL_ERROR)


class TestDeleteAudioFiles(unittest.TestCase):
    def test_deletes_audio_and_keeps_everything_else(self):
        with tempfile.TemporaryDirectory() as tmp:
            audio = ["a.flac", "b.mp3", "c.m4a"]
            other = ["folder.jpg", "album.nfo", "CORRUPT_RIP.log"]
            for name in audio + other:
                open(os.path.join(tmp, name), "w").close()

            deleted = mod.delete_audio_files(tmp)

            self.assertEqual(len(deleted), len(audio))
            for name in audio:
                self.assertFalse(os.path.exists(os.path.join(tmp, name)), name)
            for name in other:
                self.assertTrue(os.path.exists(os.path.join(tmp, name)), name)


@unittest.skipUnless(HAVE_FLAC and HAVE_FFMPEG, "needs flac and ffmpeg")
class TestEndToEnd(unittest.TestCase):
    """Drive main() as a subprocess, the way Picard invokes it."""

    def _run_script(self, folder, env=None, covit_bin="/nonexistent/covit"):
        full_env = dict(os.environ, COVIT_BIN=covit_bin)
        if env:
            full_env.update(env)
        return subprocess.run(
            [sys.executable, os.path.join(HERE, "picard_post_save.py"), folder],
            capture_output=True, text=True, env=full_env,
        )

    def test_clean_album_survives(self):
        with tempfile.TemporaryDirectory() as tmp:
            track = os.path.join(tmp, "01.flac")
            self.assertTrue(make_flac(track))
            result = self._run_script(tmp)
        self.assertEqual(result.returncode, 0)
        self.assertIn("verified clean", result.stdout)

    def test_corrupt_album_is_logged_and_deleted(self):
        with tempfile.TemporaryDirectory() as tmp:
            good = os.path.join(tmp, "01.flac")
            bad = os.path.join(tmp, "02.flac")
            art = os.path.join(tmp, "folder.jpg")
            self.assertTrue(make_flac(good))
            self.assertTrue(make_flac(bad))
            open(art, "w").close()
            corrupt_flac_payload(bad)

            result = self._run_script(tmp)

            self.assertEqual(result.returncode, 1)
            self.assertTrue(os.path.isfile(os.path.join(tmp, mod.LOG_FILENAME)))
            # The whole album goes, not just the corrupt track.
            self.assertFalse(os.path.exists(good))
            self.assertFalse(os.path.exists(bad))
            # Non-audio is left for the user to inspect.
            self.assertTrue(os.path.isfile(art))

    def test_log_only_mode_never_deletes(self):
        with tempfile.TemporaryDirectory() as tmp:
            bad = os.path.join(tmp, "01.flac")
            self.assertTrue(make_flac(bad))
            corrupt_flac_payload(bad)

            result = self._run_script(tmp, env={"RIPCHECK_LOG_ONLY": "1"})

            self.assertEqual(result.returncode, 1)
            self.assertTrue(os.path.isfile(os.path.join(tmp, mod.LOG_FILENAME)))
            self.assertTrue(
                os.path.isfile(bad), "LOG_ONLY must never delete audio"
            )

    def test_tool_error_never_deletes(self):
        """A missing flac binary must leave the album completely intact."""
        with tempfile.TemporaryDirectory() as tmp:
            track = os.path.join(tmp, "01.flac")
            self.assertTrue(make_flac(track))
            # An empty PATH makes `flac` unfindable: preflight fails, and the
            # rip check is skipped rather than treated as corruption.
            result = self._run_script(tmp, env={"PATH": ""})

            self.assertEqual(result.returncode, 0)
            self.assertTrue(
                os.path.isfile(track), "a tool error must never delete"
            )

    def test_stale_log_deletes_without_rechecking(self):
        """Documents a sharp edge: a leftover log condemns a fresh re-rip."""
        with tempfile.TemporaryDirectory() as tmp:
            track = os.path.join(tmp, "01.flac")
            self.assertTrue(make_flac(track))
            open(os.path.join(tmp, mod.LOG_FILENAME), "w").close()

            result = self._run_script(tmp)

            self.assertEqual(result.returncode, 1)
            self.assertFalse(
                os.path.exists(track),
                "known behavior: a stale CORRUPT_RIP.log deletes a good rip",
            )

    def test_empty_file_does_not_condemn_the_album(self):
        """An interrupted download beside good tracks must not delete them."""
        with tempfile.TemporaryDirectory() as tmp:
            good = os.path.join(tmp, "01.flac")
            self.assertTrue(make_flac(good))
            open(os.path.join(tmp, "02.flac"), "w").close()  # 0 bytes

            result = self._run_script(tmp)

            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue(os.path.isfile(good), "the good track was deleted")
            self.assertFalse(
                os.path.exists(os.path.join(tmp, mod.LOG_FILENAME)),
                "a tombstone was left that would condemn any re-rip",
            )

    @unittest.skipIf(os.geteuid() == 0, "root bypasses file permissions")
    def test_unreadable_file_does_not_condemn_the_album(self):
        """A permission problem must leave every good track in place."""
        with tempfile.TemporaryDirectory() as tmp:
            good = os.path.join(tmp, "01.flac")
            locked = os.path.join(tmp, "02.flac")
            self.assertTrue(make_flac(good))
            self.assertTrue(make_flac(locked))
            os.chmod(locked, 0o000)
            try:
                result = self._run_script(tmp)
                self.assertEqual(
                    result.returncode, 0, result.stdout + result.stderr
                )
                self.assertTrue(os.path.isfile(good), "the good track was deleted")
                self.assertFalse(
                    os.path.exists(os.path.join(tmp, mod.LOG_FILENAME)),
                    "a tombstone was left that would condemn any re-rip",
                )
            finally:
                os.chmod(locked, 0o644)

    def test_killed_flac_does_not_condemn_the_album(self):
        """A stubbed `flac` that dies on a signal must delete nothing."""
        with tempfile.TemporaryDirectory() as tmp:
            good = os.path.join(tmp, "01.flac")
            self.assertTrue(make_flac(good))

            bindir = os.path.join(tmp, "bin")
            os.mkdir(bindir)
            fake = os.path.join(bindir, "flac")
            with open(fake, "w") as fh:
                fh.write(
                    "#!/bin/sh\n"
                    'if [ "$1" = "--version" ]; then echo "flac 1.4"; exit 0; fi\n'
                    "kill -9 $$\n"
                )
            os.chmod(fake, 0o755)

            result = self._run_script(
                tmp, env={"PATH": bindir + os.pathsep + os.environ.get("PATH", "")}
            )

            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue(os.path.isfile(good), "the good track was deleted")
            self.assertFalse(
                os.path.exists(os.path.join(tmp, mod.LOG_FILENAME)),
                "a tombstone was left that would condemn any re-rip",
            )

    def test_missing_directory_exits_nonzero(self):
        result = self._run_script("/nonexistent/path/xyz")
        self.assertEqual(result.returncode, 1)


if __name__ == "__main__":
    unittest.main()
