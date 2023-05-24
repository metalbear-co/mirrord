import { existsSync } from "fs";
import { assert, expect } from "chai";
import { join } from "path";
import { VSBrowser, StatusBar, TextEditor, EditorView, ActivityBar, DebugView, InputBox, DebugToolbar, BottomBarPanel } from "vscode-extension-tester";
import get from "axios";


// This suite tests basic flow of mirroring traffic from remote pod
// - Enable mirrord -> Disable mirrord
// - Create mirrord config by pressing the gear icon
// - Set a breakpoint in the python file
// - Start debugging the python file
// - Select the pod from the QuickPick
// - Send traffic to the pod
// - Tests successfully exit if breakpoint is hit
// - Assert text on terminal
describe("mirrord sample flow test", function () {
    this.timeout(1000000);
    let browser: VSBrowser;
    let statusBar: StatusBar;
    let debugToolbar: DebugToolbar;
    let breakpointHit = false;
    let podToSlect = process.env.POD_TO_SELECT;
    const testWorkspace = join(__dirname, '../../test-workspace');
    const fileName = "app_flask.py";

    before(async function () {
        console.log("podToSlect: " + podToSlect);
        expect(podToSlect).to.not.be.undefined;
        browser = VSBrowser.instance;
        // need to bring the flask app in open editors
        await browser.openResources(testWorkspace, join(testWorkspace, fileName));
        await sleep(10000);
    });

    after(async function () {
        if (await debugToolbar.isDisplayed()) {
            await debugToolbar.stop();
        }
    });

    it("enable mirrord", async function (done) {
        statusBar = new StatusBar();
        const enableButton = await statusBar.getItem("Enable mirrord");
        expect(enableButton).to.not.be.undefined;
        await enableButton?.click();
        await sleep(2000);
        assert(await enableButton?.getText() === "Disable mirrord", "`Disable mirrord` button not found");
        done();
    });

    it("create mirrord config", async function (done) {        
        // gear -> $(gear) clicked to open mirrord config
        const mirrordSettingsButton = await statusBar.getItem("gear");
        expect(mirrordSettingsButton).to.not.be.undefined;
        await mirrordSettingsButton?.click();
        await browser.driver.wait(async () => {
            const mirrordConfigPath = join(__dirname, '../../test-workspace/.mirrord/mirrord.json');
            return await existsSync(mirrordConfigPath);
        }
            , 10000, "Mirrord config not found");

        done();
    });


    it("set breakpoint", async function (done) {

        const editorView = new EditorView();
        await editorView.openEditor(fileName);
        await sleep(2000);
        const currentTab = await editorView.getActiveTab();
        expect(currentTab).to.not.be.undefined;
        assert(await currentTab?.getTitle() === "app_flask.py", "app_flask.py not found");

        const textEditor = new TextEditor();
        const result = await textEditor.toggleBreakpoint(9);
        expect(result).to.be.true;
        done();
    });


    it("start debugging", async function (done) {
        const activityBar = await new ActivityBar().getViewControl("Run and Debug");
        expect(activityBar).to.not.be.undefined;
        const debugView = await activityBar?.openView() as DebugView;
        await debugView.selectLaunchConfiguration("Python: Current File");
        debugView.start();
        await sleep(10000);
        done();
    });

    it("select pod from quickpick", async function () {    
        const input = await InputBox.create();        
        // assertion that podToSlect is not undefined is done in "before" block        
        await input.selectQuickPick(podToSlect!);        
        await sleep(10000);
    });

    it("wait for breakpoint to be hit", async function (done) {
        debugToolbar = await DebugToolbar.create();
        console.log("waiting for breakpoint");

        debugToolbar.waitForBreakPoint().then(() => {
            breakpointHit = true;
            console.log("breakpoint hit");
        });
        done();
    });

    it("send traffic to pod", async function () {
        expect(breakpointHit).to.be.false;
        console.log("sending traffic to pod" + breakpointHit);
        const response = await get("http://localhost:30000");
        expect(response.status).to.equal(200);
        expect(response.data).to.equal("OK - GET: Request completed\n");
        await sleep(2000);
        assert(breakpointHit, "Breakpoint not hit");
    });

    it("assert text on terminal", async function () {
        const terminalView = await new BottomBarPanel().openTerminalView();
        const text = await terminalView.getText();
        console.log(text);
        assert(text.includes("GET: Request completed\n"));
    });

});


async function sleep(time: number) {
    await new Promise((resolve) => setTimeout(resolve, time));
}