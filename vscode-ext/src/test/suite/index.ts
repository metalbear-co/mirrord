import * as path from 'path';
import Mocha = require('mocha');
import glob from 'glob';

export function run(): Promise<void> {
  // Create the mocha test
  const mocha = new Mocha({
    ui: 'tdd',
    color: true
  });
  mocha.timeout(100000);

  const testsRoot = path.resolve(__dirname, '..');

  return new Promise((c, e) => {
    glob('**/**.test.js', { cwd: testsRoot }, (err, files) => {
      if (err) {
        return e(err);
      }

      // Add files to the test suite
      files.forEach((f: string) => mocha.addFile(path.resolve(testsRoot, f)));

      try {
        // Run the mocha test
        mocha.run((failures: number) => {
          if (failures > 0) {
            e(new Error(`${failures} tests failed.`));
          } else {
            c();
          }
        });
      } catch (err) {
        e(err);
      }
    });
  });
}