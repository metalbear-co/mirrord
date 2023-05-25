import * as path from 'path';
import { ExTester, ReleaseQuality } from 'vscode-extension-tester';

async function main(): Promise<void> {
  const version = 'latest';
  const testPath = path.join(__dirname, 'e2e.js');
  const storageFolder = path.join(__dirname, '..', 'storage');
  const extFolder = path.join(__dirname, '..', 'extensions');

  try {
    console.log(`Running tests from ${testPath}`);
    const exTester = new ExTester(storageFolder, ReleaseQuality.Stable, extFolder);
    await exTester.downloadCode(version);
    await exTester.installVsix({ useYarn: false });
    await exTester.installFromMarketplace('ms-python.python');
    await exTester.downloadChromeDriver(version);
    const result = await exTester.runTests(testPath, {
      vscodeVersion: version,
      resources: [storageFolder],
    });

    process.exit(result);
  } catch (err) {
    console.log(err);
    process.exit(1);
  }
}

main();
