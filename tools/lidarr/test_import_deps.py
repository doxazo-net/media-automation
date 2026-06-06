import os
import shutil
import sys
import tempfile
import unittest
from argparse import Namespace

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import ImportLidarrManual as mod  # noqa: E402


class TestDetectPlatform(unittest.TestCase):
    def test_mac(self):
        self.assertEqual(mod.detect_platform(platform='darwin'), 'mac')

    def test_unraid(self):
        with tempfile.NamedTemporaryFile() as marker:
            self.assertEqual(
                mod.detect_platform(platform='linux', unraid_marker=marker.name),
                'unraid',
            )

    def test_linux(self):
        self.assertEqual(
            mod.detect_platform(platform='linux2', unraid_marker='/no/such/marker'),
            'linux',
        )

    def test_unknown(self):
        self.assertEqual(
            mod.detect_platform(platform='win32', unraid_marker='/no/such/marker'),
            'unknown',
        )


class TestBuildDependencies(unittest.TestCase):
    def test_table_has_expected_deps(self):
        names = {d.name for d in mod.build_dependencies()}
        self.assertEqual(
            names,
            {'mutagen', 'essentia', 'cv2', 'magick', 'ffmpeg', 'rsgain', 'git'},
        )

    def test_kinds_and_optional_flags(self):
        by_name = {d.name: d for d in mod.build_dependencies()}
        self.assertEqual(by_name['mutagen'].kind, 'pip')
        self.assertEqual(by_name['magick'].kind, 'binary')
        self.assertTrue(by_name['cv2'].optional)
        self.assertTrue(by_name['git'].optional)
        self.assertFalse(by_name['rsgain'].optional)

    def test_pip_packages_present(self):
        by_name = {d.name: d for d in mod.build_dependencies()}
        self.assertEqual(by_name['essentia'].packages['pip'], 'essentia')
        self.assertEqual(by_name['magick'].packages['brew'], 'imagemagick')


class TestScanArtworkKinds(unittest.TestCase):
    def _album(self, files):
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d, True)
        for name in files:
            open(os.path.join(d, name), 'w').close()
        return d

    def test_detects_convertible(self):
        d = self._album(['folder.tif', 'track01.flac'])
        scan = mod.scan_artwork_kinds([d])
        self.assertTrue(scan['convertible'])
        self.assertFalse(scan['animated'])

    def test_detects_animated(self):
        d = self._album(['folder.mp4', 'track01.flac'])
        scan = mod.scan_artwork_kinds([d])
        self.assertTrue(scan['animated'])

    def test_plain_jpg_triggers_nothing(self):
        d = self._album(['folder.jpg', 'track01.flac'])
        scan = mod.scan_artwork_kinds([d])
        self.assertFalse(scan['convertible'])
        self.assertFalse(scan['animated'])


class TestNeededDependencies(unittest.TestCase):
    def setUp(self):
        # Pretend nothing is installed so every dep is "missing".
        self.deps = [
            mod.Dependency('mutagen', 'pip', lambda: False, 'x', {'pip': 'mutagen'},
                           needed_when=lambda a, s: not a.skip_bpm),
            mod.Dependency('rsgain', 'binary', lambda: False, 'x',
                           {'brew': 'rsgain'},
                           needed_when=lambda a, s: not a.no_rsgain),
            mod.Dependency('ffmpeg', 'binary', lambda: False, 'x',
                           {'brew': 'ffmpeg'},
                           needed_when=lambda a, s: s['animated']),
            mod.Dependency('git', 'binary', lambda: False, 'x',
                           {'brew': 'git'}, optional=True),
        ]

    def test_skip_bpm_drops_mutagen(self):
        args = Namespace(skip_bpm=True, no_rsgain=False)
        scan = {'convertible': False, 'animated': False}
        names = {d.name for d in mod.needed_dependencies(self.deps, args, scan)}
        self.assertNotIn('mutagen', names)
        self.assertIn('rsgain', names)

    def test_no_rsgain_drops_rsgain(self):
        args = Namespace(skip_bpm=False, no_rsgain=True)
        scan = {'convertible': False, 'animated': False}
        names = {d.name for d in mod.needed_dependencies(self.deps, args, scan)}
        self.assertNotIn('rsgain', names)

    def test_no_animated_art_drops_ffmpeg(self):
        args = Namespace(skip_bpm=False, no_rsgain=False)
        scan = {'convertible': False, 'animated': False}
        names = {d.name for d in mod.needed_dependencies(self.deps, args, scan)}
        self.assertNotIn('ffmpeg', names)

    def test_optional_never_in_needed(self):
        args = Namespace(skip_bpm=False, no_rsgain=False)
        scan = {'convertible': True, 'animated': True}
        names = {d.name for d in mod.needed_dependencies(self.deps, args, scan)}
        self.assertNotIn('git', names)


class TestInstallCommandFor(unittest.TestCase):
    def setUp(self):
        self.by_name = {d.name: d for d in mod.build_dependencies()}

    def test_pip_uses_current_interpreter(self):
        cmd = mod.install_command_for(self.by_name['mutagen'], 'unraid')
        self.assertEqual(cmd, [sys.executable, '-m', 'pip', 'install', 'mutagen'])

    def test_binary_on_mac_uses_brew(self):
        cmd = mod.install_command_for(self.by_name['magick'], 'mac')
        self.assertEqual(cmd, ['brew', 'install', 'imagemagick'])

    def test_binary_on_unraid_uses_unget(self):
        cmd = mod.install_command_for(self.by_name['ffmpeg'], 'unraid')
        self.assertEqual(cmd, ['un-get', 'install', 'ffmpeg'])

    def test_binary_on_unknown_returns_none(self):
        cmd = mod.install_command_for(self.by_name['ffmpeg'], 'unknown')
        self.assertIsNone(cmd)


class TestInstallDependency(unittest.TestCase):
    def setUp(self):
        self.dep = mod.Dependency('ffmpeg', 'binary', lambda: False, 'x',
                                  {'un-get': 'ffmpeg'})
        self._orig_run = mod.subprocess.run

    def tearDown(self):
        mod.subprocess.run = self._orig_run

    def test_success(self):
        class R:
            returncode = 0
            stderr = ''
        mod.subprocess.run = lambda *a, **k: R()
        self.assertTrue(mod.install_dependency(self.dep, 'unraid'))

    def test_nonzero_returns_false(self):
        class R:
            returncode = 1
            stderr = 'package not found'
        mod.subprocess.run = lambda *a, **k: R()
        self.assertFalse(mod.install_dependency(self.dep, 'unraid'))

    def test_missing_manager_returns_false(self):
        def boom(*a, **k):
            raise FileNotFoundError()
        mod.subprocess.run = boom
        self.assertFalse(mod.install_dependency(self.dep, 'unraid'))

    def test_print_only_platform_returns_false(self):
        self.assertFalse(mod.install_dependency(self.dep, 'unknown'))


class TestWriteIdempotentBlock(unittest.TestCase):
    def test_writes_once_then_replaces(self):
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d, True)
        path = os.path.join(d, 'go')
        with open(path, 'w') as f:
            f.write('#!/bin/bash\necho existing\n')
        mod.write_idempotent_block(path, 'TESTMARK', 'echo one')
        mod.write_idempotent_block(path, 'TESTMARK', 'echo two')
        with open(path) as f:
            content = f.read()
        self.assertEqual(content.count('# >>> TESTMARK >>>'), 1)
        self.assertEqual(content.count('# <<< TESTMARK <<<'), 1)
        self.assertIn('echo two', content)
        self.assertNotIn('echo one', content)
        self.assertIn('echo existing', content)

    def test_creates_missing_file(self):
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d, True)
        path = os.path.join(d, 'go')
        mod.write_idempotent_block(path, 'TESTMARK', 'echo hi')
        with open(path) as f:
            content = f.read()
        self.assertIn('echo hi', content)

    def test_out_of_order_markers_appended_not_corrupted(self):
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d, True)
        path = os.path.join(d, 'go')
        with open(path, 'w') as f:
            f.write('# <<< TESTMARK <<<\nstray\n# >>> TESTMARK >>>\n')
        mod.write_idempotent_block(path, 'TESTMARK', 'echo ok')
        with open(path) as f:
            content = f.read()
        # A well-formed block is appended; nothing is doubled/corrupted.
        self.assertIn('# >>> TESTMARK >>>\necho ok\n# <<< TESTMARK <<<', content)


class TestWriteBootPersistence(unittest.TestCase):
    def test_prefers_user_scripts_when_dir_exists(self):
        us_dir = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, us_dir, True)
        go_dir = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, go_dir, True)
        go_file = os.path.join(go_dir, 'go')
        dest = mod.write_boot_persistence(
            ['mutagen', 'essentia'], '/mnt/vms/x/tools/ImportLidarrManual.py',
            user_scripts_dir=us_dir, go_file=go_file)
        self.assertTrue(dest.startswith(us_dir))
        with open(dest) as f:
            content = f.read()
        self.assertIn('#!/bin/bash', content)
        self.assertIn('pip install essentia mutagen', content)
        self.assertIn('ln -sf /mnt/vms/x/tools/ImportLidarrManual.py /usr/local/bin/il',
                      content)
        self.assertFalse(os.path.exists(go_file))

    def test_falls_back_to_go_file(self):
        us_dir = os.path.join(tempfile.mkdtemp(), 'nonexistent')
        go_dir = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, go_dir, True)
        go_file = os.path.join(go_dir, 'go')
        dest = mod.write_boot_persistence(
            ['mutagen'], '/mnt/vms/x/tools/ImportLidarrManual.py',
            user_scripts_dir=us_dir, go_file=go_file)
        self.assertEqual(dest, go_file)
        with open(go_file) as f:
            content = f.read()
        self.assertIn('pip install mutagen', content)
        self.assertIn('# >>> ImportLidarrManual boot setup >>>', content)


class TestEnsureIlSymlink(unittest.TestCase):
    def test_creates_link(self):
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d, True)
        script = os.path.join(d, 'ImportLidarrManual.py')
        open(script, 'w').close()
        link = os.path.join(d, 'il')
        self.assertTrue(mod.ensure_il_symlink(script, link_path=link))
        self.assertTrue(os.path.islink(link))
        self.assertEqual(os.path.realpath(link), os.path.realpath(script))

    def test_relinks_when_target_wrong(self):
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d, True)
        script = os.path.join(d, 'ImportLidarrManual.py')
        open(script, 'w').close()
        old = os.path.join(d, 'old')
        open(old, 'w').close()
        link = os.path.join(d, 'il')
        os.symlink(old, link)
        self.assertTrue(mod.ensure_il_symlink(script, link_path=link))
        self.assertEqual(os.path.realpath(link), os.path.realpath(script))


class TestPreflightDriver(unittest.TestCase):
    def test_noninteractive_reports_without_installing(self):
        # Force a missing dep and non-interactive mode; ensure no install runs.
        deps = [mod.Dependency('rsgain', 'binary', lambda: False, 'ReplayGain',
                               {'brew': 'rsgain', 'un-get': 'rsgain'},
                               needed_when=lambda a, s: not a.no_rsgain)]
        args = Namespace(skip_bpm=False, no_rsgain=False, no_preflight=False)

        orig_build = mod.build_dependencies
        installed = []
        orig_install = mod.install_dependency
        mod.build_dependencies = lambda: deps
        mod.install_dependency = lambda d, p: installed.append(d.name)
        try:
            mod.preflight_dependencies(args, [], interactive=False)  # must not raise, must not install
        finally:
            mod.build_dependencies = orig_build
            mod.install_dependency = orig_install
        self.assertEqual(installed, [])

    def test_no_preflight_flag_short_circuits(self):
        args = Namespace(skip_bpm=False, no_rsgain=False, no_preflight=True)
        called = []
        orig = mod.build_dependencies
        mod.build_dependencies = lambda: called.append(1) or []
        try:
            mod.preflight_dependencies(args, [])
        finally:
            mod.build_dependencies = orig
        self.assertEqual(called, [])


if __name__ == '__main__':
    unittest.main()
