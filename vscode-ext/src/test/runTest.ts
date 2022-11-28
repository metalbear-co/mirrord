import { resolve } from 'path';
import { runTests } from '@vscode/test-electron';

async function main() {
    try {
        console.log("__dirname: ", resolve(__dirname, '../../'));
        const extensionDevelopmentPath = resolve(__dirname, '../../');
        const extensionTestsPath = resolve(__dirname, './suite/index');

        const launchArgs: string[] = [resolve(__dirname, '../../../tests/python-e2e')];

        await runTests({ extensionDevelopmentPath, extensionTestsPath, launchArgs });
    } catch (err) {
        console.error('Failed to run tests');
        process.exit(1);
    }
}

main();