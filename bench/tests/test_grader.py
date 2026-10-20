"""Offline checks that submitted code cannot forge its own grade."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
import arms  # noqa: E402
import run as bench  # noqa: E402

ALL_TRUE = json.dumps({'ok': True, 'checks': {name: True for name in arms.TASKS['slug-unicode']['required_checks']}, 'errors': {}})
ORACLE = (HERE / 'oracles/slug/slug.mjs').read_text()

# Each submission is wrong or never lets the grader observe a correct slug(), and tries to
# make the verifier report success anyway.
FORGERIES = {
    'exit_handler_prints_checks': 'export function slug() { return "WRONG"; }\n'
                                  f'process.on("exit", () => console.log({json.dumps(ALL_TRUE)}));\n',
    'exits_zero_at_import': f'console.log({json.dumps(ALL_TRUE)});\nprocess.exit(0);\nexport function slug() {{ return ""; }}\n',
    'exit_handler_forces_exit_zero': 'export function slug() { return "WRONG"; }\n'
                                     'process.on("exit", () => { process.exitCode = 0; });\n',
    'tampers_with_prototypes_and_json': 'export function slug() { return "WRONG"; }\n'
                                        'Array.prototype.toJSON = function () { return ["creme-brulee"]; };\n'
                                        'JSON.stringify = () => "[]";\n'
                                        'String.prototype.startsWith = () => true;\n'
                                        'Object.defineProperty(Array.prototype, "0", { set() {}, get() { return "a-b"; } });\n',
    'guesses_a_result_line': 'export function slug() { return "WRONG"; }\n'
                             'const fake = JSON.stringify(Array(18).fill("creme-brulee"));\n'
                             'for (const nonce of ["0".repeat(32), "f".repeat(32)]) console.log("\\n" + nonce + " " + fake);\n',
    'writes_to_stderr_and_stdout_late': 'export function slug() { return "WRONG"; }\n'
                                        'setTimeout(() => { process.stdout.write(' + json.dumps(ALL_TRUE + '\n') + '); }, 10);\n',
}


@unittest.skipUnless(shutil.which('node'), 'node unavailable')
class SlugGraderTest(unittest.TestCase):
    def verify(self, source):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            (directory / 'slug.mjs').write_text(source)
            record = bench.invoke([sys.executable, str(HERE / 'verify.py'), 'slug-unicode', str(directory)], directory,
                                  os.environ, 60, directory / 'grade-verify')
            accepted, result = bench.acceptance('slug-unicode', record, directory)
            return record, accepted, result

    def test_forged_output_never_passes(self):
        for name, source in FORGERIES.items():
            with self.subTest(name):
                record, accepted, result = self.verify(source)
                self.assertNotEqual(record['exit_code'], 0)
                self.assertFalse(accepted)
                self.assertFalse(result['ok'])

    def test_review_counterexample_fails_its_cases(self):
        # The grader observed "WRONG" for every case. (A constant is idempotent, so that check holds.)
        _, accepted, result = self.verify(FORGERIES['exit_handler_prints_checks'])
        self.assertFalse(accepted)
        self.assertEqual({name for name, value in result['checks'].items() if value}, {'idempotent'})
        self.assertEqual(result['errors'], {})

    def test_tampering_does_not_stop_the_grader_observing_results(self):
        _, _, result = self.verify(FORGERIES['tampers_with_prototypes_and_json'])
        self.assertEqual(result['errors'], {})
        self.assertFalse(any(result['checks'][f'case_{i}'] for i in range(8)))

    def test_a_correct_submission_that_prints_noise_still_passes(self):
        noisy = ORACLE + '\nconsole.log("loaded");\nconsole.error("warning");\nconsole.log(' + json.dumps(ALL_TRUE) + ');\n'
        record, accepted, result = self.verify(noisy)
        self.assertEqual(record['exit_code'], 0, result)
        self.assertTrue(accepted)

    def test_wrong_on_one_hidden_case_fails_only_that_case(self):
        source = ORACLE.replace('export function slug(text) {', 'export function slug(text) {\n  if (text === "version 42") return "v42";')
        _, accepted, result = self.verify(source)
        self.assertFalse(accepted)
        self.assertEqual([name for name, value in result['checks'].items() if not value], ['case_5'])


class AcceptanceLinesTest(unittest.TestCase):
    def test_a_second_result_line_rejects_the_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            (directory / 'v.stdout').write_text(ALL_TRUE + '\n' + ALL_TRUE + '\n')
            receipt = {'exit_code': 0, 'timed_out': False, 'receipt': 'v'}
            self.assertFalse(bench.acceptance('slug-unicode', receipt, directory)[0])
            (directory / 'v.stdout').write_text('stack: ready\n' + ALL_TRUE + '\n')
            self.assertTrue(bench.acceptance('slug-unicode', receipt, directory)[0])


if __name__ == '__main__':
    unittest.main()
