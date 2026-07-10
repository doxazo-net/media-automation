import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import ImportLidarrManual as mod  # noqa: E402


def _touch(directory, name):
    path = os.path.join(directory, name)
    with open(path, 'w') as fh:
        fh.write('x')
    return path


class TestFindSidecars(unittest.TestCase):
    def test_finds_lyrics_and_nfo_and_cue_excludes_audio_and_art(self):
        with tempfile.TemporaryDirectory() as d:
            _touch(d, '01 - Song.flac')      # audio -> excluded
            _touch(d, 'folder.jpg')          # artwork -> excluded
            lrc = _touch(d, '01 - Song.lrc')
            txt = _touch(d, '01 - Song.txt')
            nfo = _touch(d, '01 - Song.nfo')
            cue = _touch(d, 'Album.cue')
            found = mod.find_sidecars(d)
            self.assertEqual(sorted(found), sorted([lrc, txt, nfo, cue]))

    def test_missing_dir_returns_empty(self):
        self.assertEqual(mod.find_sidecars('/no/such/dir'), [])


class TestClassifySidecars(unittest.TestCase):
    def test_pertrack_vs_album(self):
        audio = ['/m/01 - Song.flac', '/m/02 - Other.flac']
        sidecars = [
            '/m/01 - Song.lrc',   # matches track -> per-track
            '/m/02 - Other.nfo',  # matches track -> per-track
            '/m/album.nfo',       # no track match, album ext -> album
            '/m/Album.cue',       # album ext -> album
            '/m/readme.txt',      # no track match, txt not album ext -> dropped
        ]
        pertrack, album = mod.classify_sidecars(sidecars, audio)
        self.assertEqual(sorted(pertrack), ['/m/01 - Song.lrc', '/m/02 - Other.nfo'])
        self.assertEqual(sorted(album), ['/m/Album.cue', '/m/album.nfo'])

    def test_case_insensitive_basename_match(self):
        audio = ['/m/Track One.flac']
        pertrack, album = mod.classify_sidecars(['/m/track one.lrc'], audio)
        self.assertEqual(pertrack, ['/m/track one.lrc'])
        self.assertEqual(album, [])


class TestExtraExtensionHelpers(unittest.TestCase):
    def test_needed_finds_missing(self):
        sidecars = ['/m/01.lrc', '/m/01.nfo', '/m/02.lrc']
        self.assertEqual(mod.needed_extra_extensions(sidecars, 'nfo'), ['lrc'])

    def test_needed_empty_when_all_present(self):
        self.assertEqual(mod.needed_extra_extensions(['/m/01.lrc'], 'lrc,nfo'), [])

    def test_needed_handles_empty_csv(self):
        self.assertEqual(mod.needed_extra_extensions(['/m/01.lrc'], ''), ['lrc'])

    def test_union_appends_new(self):
        self.assertEqual(mod.union_extra_extensions('srt,nfo', ['lrc']), 'srt,nfo,lrc')

    def test_union_dedupes_case_insensitively(self):
        self.assertEqual(mod.union_extra_extensions('NFO', ['nfo', 'lrc']), 'NFO,lrc')

    def test_union_into_empty(self):
        self.assertEqual(mod.union_extra_extensions('', ['lrc', 'nfo']), 'lrc,nfo')


class TestDecideConfigAction(unittest.TestCase):
    def test_needs_change_when_off(self):
        cfg = {'importExtraFiles': False, 'extraFileExtensions': 'lrc,nfo'}
        needs, off, missing = mod.decide_config_action(cfg, ['/m/01.lrc'])
        self.assertTrue(needs)
        self.assertTrue(off)
        self.assertEqual(missing, [])

    def test_needs_change_when_ext_missing(self):
        cfg = {'importExtraFiles': True, 'extraFileExtensions': 'nfo'}
        needs, off, missing = mod.decide_config_action(cfg, ['/m/01.lrc'])
        self.assertTrue(needs)
        self.assertFalse(off)
        self.assertEqual(missing, ['lrc'])

    def test_no_change_when_ready(self):
        cfg = {'importExtraFiles': True, 'extraFileExtensions': 'lrc,nfo'}
        needs, off, missing = mod.decide_config_action(cfg, ['/m/01.lrc'])
        self.assertFalse(needs)


class TestClientConfigMethods(unittest.TestCase):
    def test_get_and_put_hit_expected_endpoints(self):
        calls = []

        class FakeClient(mod.LidarrClient):
            def __init__(self):
                pass  # skip real __init__ (no network)

            def get(self, endpoint, params=None, **kw):
                calls.append(('GET', endpoint))
                return {'importExtraFiles': False, 'extraFileExtensions': 'nfo'}

            def put(self, endpoint, data, **kw):
                calls.append(('PUT', endpoint, data))
                return data

        c = FakeClient()
        cfg = c.get_media_management_config()
        self.assertEqual(cfg['extraFileExtensions'], 'nfo')
        c.update_media_management_config({'importExtraFiles': True})
        self.assertEqual(calls[0], ('GET', 'config/mediamanagement'))
        self.assertEqual(calls[1][:2], ('PUT', 'config/mediamanagement'))


class TestPreserveSidecarsFlag(unittest.TestCase):
    def setUp(self):
        # parse_args validates Lidarr URL/API key are resolvable; supply
        # dummy values via env vars so these tests exercise flag parsing
        # in isolation (same pattern as TestScaffoldEnvFile in
        # test_import_deps.py).
        for var, dummy in (('LIDARR_URL', 'http://dummy:8686'),
                           ('LIDARR_API_KEY', 'dummy')):
            orig = os.environ.get(var)
            os.environ[var] = dummy
            self.addCleanup(
                lambda v=var, o=orig: (os.environ.__setitem__(v, o)
                                        if o is not None
                                        else os.environ.pop(v, None)))

    def test_flag_defaults_false(self):
        args = mod.parse_args(['/some/path'])
        self.assertFalse(args.preserve_sidecars)

    def test_flag_sets_true(self):
        args = mod.parse_args(['/some/path', '--preserve-sidecars'])
        self.assertTrue(args.preserve_sidecars)


class TestEnsureExtraFilesConfig(unittest.TestCase):
    def _client(self, cfg, put_sink):
        class FakeClient(mod.LidarrClient):
            def __init__(self):
                pass

            def get_media_management_config(self):
                return dict(cfg)

            def update_media_management_config(self, new):
                put_sink.append(new)
                return new
        return FakeClient()

    def test_enables_and_unions_on_yes(self):
        put_sink = []
        c = self._client({'importExtraFiles': False, 'extraFileExtensions': 'nfo'}, put_sink)
        ok = mod.ensure_extra_files_config(c, ['/m/01.lrc'], dry_run=False,
                                           prompt=lambda _p: 'y')
        self.assertTrue(ok)
        self.assertEqual(len(put_sink), 1)
        self.assertTrue(put_sink[0]['importExtraFiles'])
        self.assertIn('lrc', put_sink[0]['extraFileExtensions'].split(','))
        self.assertIn('nfo', put_sink[0]['extraFileExtensions'].split(','))

    def test_declined_makes_no_put(self):
        put_sink = []
        c = self._client({'importExtraFiles': False, 'extraFileExtensions': ''}, put_sink)
        ok = mod.ensure_extra_files_config(c, ['/m/01.lrc'], dry_run=False,
                                           prompt=lambda _p: 'n')
        self.assertFalse(ok)
        self.assertEqual(put_sink, [])

    def test_ready_config_no_put(self):
        put_sink = []
        c = self._client({'importExtraFiles': True, 'extraFileExtensions': 'lrc,nfo'}, put_sink)
        ok = mod.ensure_extra_files_config(c, ['/m/01.lrc'], dry_run=False,
                                           prompt=lambda _p: 'n')
        self.assertTrue(ok)
        self.assertEqual(put_sink, [])

    def test_dry_run_no_put_but_ready(self):
        put_sink = []
        c = self._client({'importExtraFiles': False, 'extraFileExtensions': ''}, put_sink)
        ok = mod.ensure_extra_files_config(c, ['/m/01.lrc'], dry_run=True,
                                           prompt=lambda _p: 'y')
        self.assertTrue(ok)
        self.assertEqual(put_sink, [])


class TestPreserveSidecarsPreimport(unittest.TestCase):
    def _ready_client(self):
        class FakeClient(mod.LidarrClient):
            def __init__(self):
                pass

            def get_media_management_config(self):
                return {'importExtraFiles': True, 'extraFileExtensions': 'lrc,txt,nfo'}
        return FakeClient()

    def test_exact_match_carried_leftovers_warned(self):
        with tempfile.TemporaryDirectory() as d:
            _touch(d, '01 - Song.flac')
            match = _touch(d, '01 - Song.lrc')      # exact match -> carried
            stray = _touch(d, 'mystery.lrc')        # no match -> leftover
            album = _touch(d, 'album.nfo')          # album-level -> leftover
            audio = [os.path.join(d, '01 - Song.flac')]
            summary = mod.preserve_sidecars_preimport(
                self._ready_client(), d, audio, dry_run=False, prompt=lambda _p: 'y')
            self.assertEqual(summary['pertrack'], [match])
            self.assertEqual(sorted(summary['leftovers']), sorted([stray, album]))
            self.assertTrue(summary['config_ready'])

    def test_no_sidecars_is_noop(self):
        with tempfile.TemporaryDirectory() as d:
            _touch(d, '01 - Song.flac')
            audio = [os.path.join(d, '01 - Song.flac')]
            summary = mod.preserve_sidecars_preimport(
                self._ready_client(), d, audio, dry_run=False, prompt=lambda _p: 'y')
            self.assertEqual(summary['pertrack'], [])
            self.assertEqual(summary['leftovers'], [])


class TestVerifySidecarsLanded(unittest.TestCase):
    def _client_with_dest(self, dest):
        class FakeClient(mod.LidarrClient):
            def __init__(self):
                pass

            def get_track_files(self, album_id):
                return [{'path': os.path.join(dest, '1 - Renamed.flac')}]
        return FakeClient()

    def test_all_landed_no_shortfall(self):
        with tempfile.TemporaryDirectory() as dest:
            _touch(dest, '1 - Renamed.flac')
            _touch(dest, '1 - Renamed.lrc')          # the carried sidecar, renamed by Lidarr
            pertrack = ['/src/01 - Song.lrc']         # 1 x .lrc carried
            missing = mod.verify_sidecars_landed(
                self._client_with_dest(dest), 5, pertrack, '', '')
            self.assertEqual(missing, [])

    def test_shortfall_reported(self):
        with tempfile.TemporaryDirectory() as dest:
            _touch(dest, '1 - Renamed.flac')          # no .lrc landed
            pertrack = ['/src/01 - Song.lrc']
            missing = mod.verify_sidecars_landed(
                self._client_with_dest(dest), 5, pertrack, '', '')
            self.assertEqual(len(missing), 1)
            self.assertIn('.lrc', missing[0])


if __name__ == '__main__':
    unittest.main()
