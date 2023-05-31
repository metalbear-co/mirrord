import { existsSync, stat } from "fs";
import { assert, expect } from "chai";
import { join } from "path";
import { VSBrowser, StatusBar, TextEditor, EditorView, ActivityBar, DebugView, InputBox, DebugToolbar, Workbench, WebElement } from "vscode-extension-tester";
import get from "axios";
import { Breakpoint } from "vscode";


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

describe("mirrord sample flow test", function () {

    this.timeout(1000000); // --> mocha tests timeout
    this.bail(true); // --> stop tests on first failure

    let browser: VSBrowser;
    let statusBar: StatusBar;
    let debugToolbar: DebugToolbar;

    const testWorkspace = join(__dirname, '../../test-workspace');
    const fileName = "app_flask.py";
    const mirrordConfigPath = join(testWorkspace, '.mirrord/mirrord.json');
    const configurationFile = "Python: Current File";
    const breakPoint = 9;

    before(async function () {
        console.log("podToSelect: " + podToSelect);
        console.log("kubeService: " + kubeService);
        expect(podToSelect).to.not.be.undefined;
        expect(kubeService).to.not.be.undefined;
        browser = VSBrowser.instance;
        // need to bring the flask app in open editors
        await browser.openResources(testWorkspace, join(testWorkspace, fileName));
        await browser.waitForWorkbench();
    });

    after(async function () {
        if (await debugToolbar?.isDisplayed()) {
            await debugToolbar.stop();
        }
    });

    it("enable mirrord", async function () {
        statusBar = await new StatusBar();
        await browser.driver.wait(async () => {
            const enableButton = await statusBar.getItem("Enable mirrord");
            if (enableButton !== undefined) {
                enableButton.click();
                return true;
            }
            return false;
        }, 10000, "mirrord `enable` button not found -- timed out");

        await browser.driver.wait(async () => {
            const enableButton = await statusBar.getItem("Disable mirrord");
            if (enableButton !== undefined) {
                return true;
            }
            return false;
        }, 10000, "Mirrord not enabled -- timed out");
    });

    it("create mirrord config", async function () {
        // gear -> $(gear) clicked to open mirrord config
        await browser.driver.wait(async () => {
            const mirrordSettingsButton = await statusBar.getItem("gear");
            if (mirrordSettingsButton !== undefined) {
                mirrordSettingsButton.click();
                return true;
            }
            return false;
        }, 10000, "mirrord config `$(gear)` button not found -- timed out");

        await browser.driver.wait(async () => {
            return await existsSync(mirrordConfigPath);
        }, 10000, "mirrord `default` config not found");
    });

    it("select pod from quickpick", async function () {
        await setBreakPoint(fileName, breakPoint, browser);
        await startDebugging(configurationFile);

        const inputBox = await InputBox.create();
        // assertion that podToSelect is not undefined is done in "before" block   
        await browser.driver.wait(async () => {
            return await inputBox.isDisplayed();
        }, 10000, "quickPick not found -- timed out");
        await inputBox.selectQuickPick(podToSelect!);
    });

    it("wait for breakpoint to be hit", async function () {
        debugToolbar = await DebugToolbar.create(10000);
        // waiting for breakpoint and sending traffic to pod are run in parallel
        // however, traffic is sent after 10 seconds that we are sure the IDE is listening
        // for breakpoints
        await browser.driver.wait(async () => {
            return await debugToolbar.isDisplayed();
        }, 20000, "debug toolbar not found -- timed out");

        await Promise.all([debugToolbar.waitForBreakPoint(), sendTrafficToPod()]);
    });

});

// This promise is run in parallel to the promise waiting for the breakpoint to be hit
// We wait for 10 seconds to make sure that we are in listening state
async function sendTrafficToPod() {
    await new Promise(resolve => setTimeout(resolve, 10000));
    const response = await get(kubeService!!);
    expect(response.status).to.equal(200);
    expect(response.data).to.equal("OK - GET: Request completed\n");
}

// opens and sets a breakpoint in the given file
async function setBreakPoint(fileName: string, breakPoint: number, browser: VSBrowser) {
    const editorView = new EditorView();
    await editorView.openEditor(fileName);
    const currentTab = await editorView.getActiveTab();
    expect(currentTab).to.not.be.undefined;
    await browser.driver.wait(async () => {
        const tabTitle = await currentTab?.getTitle();
        if (tabTitle !== undefined) {
            return tabTitle === fileName;
        }
        return false;
    }, 10000, "editor tab title not found -- timed out");

    const textEditor = new TextEditor();
    const result = await textEditor.toggleBreakpoint(breakPoint);
    expect(result).to.be.true;
}

// starts debugging the current file with the provided configuration
// debugging starts from the "Run and Debug" button in the activity bar
async function startDebugging(configurationFile: string) {
    const activityBar = await new ActivityBar().getViewControl("Run and Debug");
    expect(activityBar).to.not.be.undefined;
    const debugView = await activityBar?.openView() as DebugView;
    await debugView.selectLaunchConfiguration(configurationFile);
    debugView.start();
}