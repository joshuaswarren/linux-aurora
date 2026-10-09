import unittest
from cts_cache import resolve_cache_policy, shader_cache_environment


class CacheIsolationTests(unittest.TestCase):
    def test_normal_iterations_cache_but_trace_must_compile(self):
        self.assertEqual(resolve_cache_policy('auto'), 'isolated')
        self.assertEqual(resolve_cache_policy('auto', trace=True), 'off')
        self.assertEqual(resolve_cache_policy('off'), 'off')
        with self.assertRaises(ValueError):
            resolve_cache_policy('isolated', trace=True)

    def test_disabled_policy_cannot_use_a_persistent_directory(self):
        self.assertEqual(shader_cache_environment('off', 'a' * 64, 'b' * 64),
                         {'MESA_SHADER_CACHE_DISABLE': 'true'})

    def test_same_builds_reuse_cache_across_test_labels(self):
        first = shader_cache_environment('isolated', 'a' * 64, 'b' * 64)
        self.assertEqual(first, shader_cache_environment('isolated', 'a' * 64, 'b' * 64))
        self.assertEqual(first['MESA_SHADER_CACHE_DISABLE'], 'false')
        self.assertEqual(first['MESA_SHADER_CACHE_SHOW_STATS'], 'true')

    def test_either_binary_change_separates_the_cache(self):
        pairs = [('a' * 64, 'b' * 64), ('c' * 64, 'b' * 64), ('a' * 64, 'c' * 64)]
        directories = {shader_cache_environment('isolated', *pair)['MESA_SHADER_CACHE_DIR']
                       for pair in pairs}
        self.assertEqual(len(directories), 3)

    def test_invalid_identity_cannot_fall_back_to_shared_cache(self):
        for policy, mesa, kernel in [('auto', 'a' * 64, 'b' * 64),
                                     ('isolated', '../shared', 'b' * 64),
                                     ('isolated', 'a' * 64, 'b' * 63)]:
            with self.assertRaises(ValueError):
                shader_cache_environment(policy, mesa, kernel)


if __name__ == '__main__':
    unittest.main()
