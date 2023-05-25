import { existsSync } from 'fs';
import { assert, config, expect } from 'chai';
import { join } from 'path';
import { VSBrowser, StatusBar, TextEditor, EditorView, ActivityBar, DebugView, InputBox, DebugToolbar } from 'vscode-extension-tester';
import get from 'axios';

// This suite tests basic flow of mirroring traffic from remote pod
// - Enable mirrord -> Disable mirrord
// - Create mirrord config by pressing the gear icon
// - Set a breakpoint in the python file
// - Start debugging the python file
// - Select the pod from the QuickPick
// - Send traffic to the pod
// - Tests successfully exit if breakpoint is hit
const kubeService = process.env.KUBE_SERVICE;
const podToSelect = process.env.POD_TO_SELECT;

describe('mirrord sample flow test', function () {
  this.timeout(1000000); // --> mocha tests timeout

  let browser: VSBrowser;
  let statusBar: StatusBar;
  let debugToolbar: DebugToolbar;

  const testWorkspace = join(__dirname, '../../test-workspace');
  const fileName = 'app_flask.py';
  const mirrordConfigPath = join(testWorkspace, '.mirrord/mirrord.json');
  const configurationFile = 'Python: Current File';
  const breakpointLine = 9;

  before(async function () {
    console.log('podToSelect: ' + podToSelect);
    console.log('kubeService: ' + kubeService);
    expect(podToSelect).to.not.be.undefined;
    expect(kubeService).to.not.be.undefined;
    browser = VSBrowser.instance;
    // need to bring the flask app in open editors
    await browser.openResources(testWorkspace, join(testWorkspace, fileName));
    await sleep(10000); // --> wait for the IDE to load
  });

  after(async function () {
    // stop debugging
    if (await debugToolbar.isDisplayed()) {
      await debugToolbar.stop();
    }
  });

  it('Enable mirrord', async function () {
    statusBar = new StatusBar();
    const enableButton = await statusBar.getItem('Enable mirrord');
    expect(enableButton).to.not.be.undefined;
    await enableButton?.click();
    await sleep(2000);
    assert((await enableButton?.getText()) === 'Disable mirrord', '`Disable mirrord` button not found');
  });

  it('Create mirrord config', async function () {
    // gear -> $(gear) clicked to open mirrord config
    const mirrordSettingsButton = await statusBar.getItem('gear');
    expect(mirrordSettingsButton).to.not.be.undefined;
    await mirrordSettingsButton?.click();
    await browser.driver.wait(
      async () => {
        return await existsSync(mirrordConfigPath);
      },
      10000,
      'Mirrord config not found'
    );
  });

  it('Select pod from quickpick', async function () {
    await setBreakPoint(fileName, breakpointLine);
    await startDebugging(configurationFile);

    const input = await InputBox.create();
    // assertion that podToSelect is not undefined is done in "before" block
    await sleep(5000); // --> wait for the quickpick dialog to load
    await input.selectQuickPick(podToSelect!);
  });

  it('Wait for breakpoint to be hit', async function () {
    await sleep(10000); // --> wait for mirrord to start
    debugToolbar = await DebugToolbar.create();

    // waiting for breakpoint and sending traffic to pod are run in parallel
    // however, traffic is sent after 10 seconds that we are sure the IDE is listening
    // for breakpoints
    await Promise.all([debugToolbar.waitForBreakPoint(), sendTrafficToPod()]);
  });
});

// utility function to wait for a given time
async function sleep(time: number) {
  await new Promise((resolve) => setTimeout(resolve, time));
}

// This promis is run in parallel to the promise waiting for the breakpoint to be hit
// We wait for 10 seconds to make sure that we are in listening state
async function sendTrafficToPod() {
  await sleep(10000);
  const response = await get(kubeService!!);
  expect(response.status).to.equal(200);
  expect(response.data).to.equal('OK - GET: Request completed\n');
}

// opens and sets a breakpoint in the given file
async function setBreakPoint(fileName: string, lineNumber: number) {
  const editorView = new EditorView();
  await editorView.openEditor(fileName);
  await sleep(5000);
  const currentTab = await editorView.getActiveTab();
  expect(currentTab).to.not.be.undefined;
  assert((await currentTab?.getTitle()) === fileName, '${fileName} not found');

  const textEditor = new TextEditor();
  const result = await textEditor.toggleBreakpoint(lineNumber);
  expect(result).to.be.true;
}

// starts debugging the current file with the provided configuration
// debugging starts from the "Run and Debug" button in the activity bar
async function startDebugging(configurationFile: string) {
  const activityBar = await new ActivityBar().getViewControl('Run and Debug');
  expect(activityBar).to.not.be.undefined;
  const debugView = (await activityBar?.openView()) as DebugView;
  await debugView.selectLaunchConfiguration(configurationFile);
  debugView.start();
  await sleep(10000);
}
