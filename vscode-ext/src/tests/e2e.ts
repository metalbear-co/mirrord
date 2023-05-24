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

    it("enable mirrord", async function () {
        statusBar = new StatusBar();
        const enableButton = await statusBar.getItem("Enable mirrord");
        expect(enableButton).to.not.be.undefined;
        await enableButton?.click();
        assert(await enableButton?.getText() === "Disable mirrord", "`Disable mirrord` button not found");
    });

    it("create mirrord config", async function () {
        this.timeout(100000);
        // gear -> $(gear) clicked to open mirrord config
        const mirrordSettingsButton = await statusBar.getItem("gear");
        expect(mirrordSettingsButton).to.not.be.undefined;

        await mirrordSettingsButton?.click();
        await browser.driver.wait(async () => {
            const mirrordConfigPath = join(__dirname, '../../test-workspace/.mirrord/mirrord.json');
            return await existsSync(mirrordConfigPath);
        }
            , 10000, "Mirrord config not found");
    });


    it("set breakpoint", async function () {
        const editorView = new EditorView();
        await editorView.openEditor(fileName);
        const currentTab = await editorView.getActiveTab();
        expect(currentTab).to.not.be.undefined;
        assert(await currentTab?.getTitle() === "app_flask.py", "app_flask.py not found");

        const textEditor = new TextEditor();
        const result = await textEditor.toggleBreakpoint(9);
        expect(result).to.be.true;
    });


    it("start debugging", async function () {
        const activityBar = await new ActivityBar().getViewControl("Run and Debug");
        expect(activityBar).to.not.be.undefined;
        const debugView = await activityBar?.openView() as DebugView;
        await debugView.selectLaunchConfiguration("Python: Current File");
        debugView.start();
    });

    it("select pod from quickpick", async function () {
        console.log("creating inputbox")
        const input = await InputBox.create();
        console.log("podToSlect: " + podToSlect)
        // assertion that podToSlect is not undefined is done in "before" block
        const picks = await input.getQuickPicks()
        for (const i of picks) {
            console.log("pick: " + await i.getLabel());
        }
        await input.selectQuickPick(podToSlect!);
        console.log("pod selected");
        await sleep(10000);
    });

    it("wait for breakpoint to be hit", async function () {
        debugToolbar = await DebugToolbar.create();
        console.log("waiting for breakpoint");

        debugToolbar.waitForBreakPoint().then(() => {
            breakpointHit = true;
            console.log("breakpoint hit");
        });
    });

    it("send traffic to pod", async function () {
        expect(breakpointHit).to.be.false;
        const response = await get("http://localhost:30000");
        expect(response.status).to.equal(200);
        expect(response.data).to.equal("OK - GET: Request completed\n");
        await sleep(2000);
        assert(breakpointHit, "Breakpoint not hit");
    });

    it("assert text on terminal", async function () {
        const terminalView = await new BottomBarPanel().openTerminalView();
        const text = await terminalView.getText();
        assert(text.includes("GET: Request completed\n"));
    });

});


async function sleep(time: number) {
    await new Promise((resolve) => setTimeout(resolve, time));
}