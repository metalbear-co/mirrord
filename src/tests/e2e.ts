import { existsSync } from "fs";
import { expect } from "chai";
import { join } from "path";
import { VSBrowser, StatusBar, TextEditor, EditorView, ActivityBar, DebugView, InputBox, DebugToolbar } from "vscode-extension-tester";
import get from "axios";


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

    const testWorkspace = join(__dirname, '../../test-workspace');
    const fileName = "app_flask.py";
    const mirrordConfigPath = join(testWorkspace, '.mirrord/mirrord.json');
    const defaultTimeout = 10000;

    before(async function () {
        console.log("podToSelect: " + podToSelect);
        console.log("kubeService: " + kubeService);

        expect(podToSelect).to.not.be.undefined;
        expect(kubeService).to.not.be.undefined;

        browser = VSBrowser.instance;
        // need to bring the flask app in open editors
        await browser.openResources(testWorkspace, join(testWorkspace, fileName));
    });
    
    it("enable mirrord", async function () {
        const statusBar = new StatusBar();
        await browser.driver.wait(async () => {
            const enableButton = await statusBar.getItem("Enable mirrord");
            if (enableButton !== undefined) {
                enableButton.click();
                return true;
            }
        }, defaultTimeout, "mirrord `enable` button not found -- timed out");

        await browser.driver.wait(async () => {
            const enableButton = await statusBar.getItem("Disable mirrord");
            if (enableButton !== undefined) {
                return true;
            }
        }, defaultTimeout, "mirrord `disable` button not found -- timed out");
    });

    it("create mirrord config", async function () {
        // gear -> $(gear) clicked to open mirrord config
        const statusBar = new StatusBar();
        await browser.driver.wait(async () => {
            const mirrordSettingsButton = await statusBar.getItem("gear");
            if (mirrordSettingsButton !== undefined) {
                mirrordSettingsButton.click();
                return true;
            }
        }, defaultTimeout, "mirrord config `$(gear)` button not found -- timed out");

        await browser.driver.wait(async () => {
            return await existsSync(mirrordConfigPath);
        }, defaultTimeout, "mirrord `default` config not found");
    });

    it("select pod from quickpick", async function () {
        await setBreakPoint(fileName, browser, defaultTimeout);
        await startDebugging();

        const inputBox = await InputBox.create();
        // assertion that podToSelect is not undefined is done in "before" block   
        await browser.driver.wait(async () => {
            return await inputBox.isDisplayed();
        }, defaultTimeout, "quickPick not found -- timed out");
        await inputBox.selectQuickPick(podToSelect!);
    });

    it("wait for breakpoint to be hit", async function () {
        const debugToolbar = await DebugToolbar.create(2 * defaultTimeout);
        // waiting for breakpoint and sending traffic to pod are run in parallel
        // however, traffic is sent after 10 seconds that we are sure the IDE is listening
        // for breakpoints
        await browser.driver.wait(async () => {
            return await debugToolbar.isDisplayed();
        }, 2 * defaultTimeout, "debug toolbar not found -- timed out");

        sendTrafficToPod(debugToolbar);
        debugToolbar.waitForBreakPoint();
    });
});

async function sendTrafficToPod(debugToolbar: DebugToolbar) {
    const response = await get(kubeService!!);
    expect(response.status).to.equal(200);
    expect(response.data).to.equal("OK - GET: Request completed\n");
}

// opens and sets a breakpoint in the given file
async function setBreakPoint(fileName: string, browser: VSBrowser, timeout: number, breakPoint: number = 9) {
    const editorView = new EditorView();
    await editorView.openEditor(fileName);
    const currentTab = await editorView.getActiveTab();
    expect(currentTab).to.not.be.undefined;
    await browser.driver.wait(async () => {
        const tabTitle = await currentTab?.getTitle();
        if (tabTitle !== undefined) {
            return tabTitle === fileName;
        }
    }, timeout, "editor tab title not found -- timed out");

    const textEditor = new TextEditor();
    const result = await textEditor.toggleBreakpoint(breakPoint);
    expect(result).to.be.true;
}

// starts debugging the current file with the provided configuration
// debugging starts from the "Run and Debug" button in the activity bar
async function startDebugging(configurationFile: string = "Python: Current File") {
    const activityBar = await new ActivityBar().getViewControl("Run and Debug");
    expect(activityBar).to.not.be.undefined;
    const debugView = await activityBar?.openView() as DebugView;
    await debugView.selectLaunchConfiguration(configurationFile);
    debugView.start();
}