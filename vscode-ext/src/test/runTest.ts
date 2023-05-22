import { resolve } from 'path';
import { runTests } from '@vscode/test-electron';

async function main() {
    try {        
        const extensionDevelopmentPath = resolve(__dirname, '../../');
        const extensionTestsPath = resolve(__dirname, './suite/index');

        const launchArgs: string[] = [resolve(__dirname, './test_workspace')];

        await runTests({ extensionDevelopmentPath, extensionTestsPath, launchArgs });
    } catch (err) {
        console.error('Failed to run tests:\n', err);
        process.exit(1);
    }
}

main();