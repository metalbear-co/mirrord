import { existsSync } from "fs";
import { assert, expect } from "chai"
import { join } from "path";
import { VSBrowser, StatusBar, TextEditor, EditorView, ActivityBar, DebugView, InputBox, DebugToolbar } from "vscode-extension-tester";
import  get  from "axios"


// This suite tests basic flow of mirroring traffic from remote pod
// - Enable mirrord -> Disable mirrord
// - Create mirrord config by pressing the gear icon
// - Set a breakpoint in the python file
// - Start debugging the python file
// - Select the pod from the QuickPick
// - Send traffic to the pod
// - Verify that the breakpoint is hit
describe("mirrord sample flow test", function () {
    console.log("Initializing workspace");
    this.timeout(10000000000);
    let browser: VSBrowser;

    before(async function () {
        browser = VSBrowser.instance;
        await browser.openResources(join(__dirname, '../../test-workspace'), join(__dirname, '../../test-workspace/app_flask.py'));
        await sleep(10000);
    });

    it("enable mirrord", async function () {
        const statusBar = new StatusBar();
        const enableButton = await statusBar.getItem("Enable mirrord");
        assert(enableButton !== undefined, "Enable mirrord button not found");
        await enableButton?.click();
        assert(await enableButton?.getText() == "Disable mirrord", "Disable mirrord button not found");
    });

    it("create mirrord config", async function () {
        this.timeout(1000000);
        const statusBar = new StatusBar();
        const mirrordSettingsButton = await statusBar.getItem("gear");
        assert(mirrordSettingsButton !== undefined, "Mirrord settings button not found");

        await mirrordSettingsButton?.click();
        await browser.driver.wait(async () => {
            const mirrordConfigPath = join(__dirname, '../../test-workspace/.mirrord/mirrord.json');
            return await existsSync(mirrordConfigPath);
        }
            , 100000, "Mirrord config not found");
    });


    it("set breakpoint", async function () {
        const editorView = new EditorView();
        await editorView.openEditor("app_flask.py");

        const textEditor = new TextEditor();
        const result = await textEditor.toggleBreakpoint(30);
        expect(result).to.be.true;
    });


    it("start debugging", async function () {
        const activityBar = await new ActivityBar().getViewControl("Run and Debug");
        const view = await activityBar?.openView() as DebugView;
        await view.selectLaunchConfiguration("Python: Current File");
        view.start();
    });

    it("select pod from quickpick", async function () {
        const input = await InputBox.create();
        const picks = await input.selectQuickPick("pod/py-serv-deployment-ff89b5974-dhl2q")
        await sleep(10000);
    });

    it("wait for breakpoint to be hit", async function () {
        const debugBar = await DebugToolbar.create();
        console.log("waiting for breakpoint");
        await debugBar.waitForBreakPoint();
        // todo: now need to figure how to wait on the breakpoint and send traffic      
    });

});


async function sleep(time: number) {
    await new Promise((resolve) => setTimeout(resolve, time));
}