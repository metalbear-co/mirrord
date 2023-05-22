import { assert } from "console";
import { stat } from "fs";
import { join } from "path";
import { By, VSBrowser, EditorView, WebView, Workbench, Notification, StatusBar, NotificationType } from "vscode-extension-tester";


describe("mirrord sample flow test", function() {
    console.log("Initializing workspace");
    this.timeout(10000);
    let browser: VSBrowser;    
  
    before(async function() {
        browser = VSBrowser.instance;
        await browser.openResources(join(__dirname, '../../test-workspace'));
        await sleep(3000);
    });

    it("enable mirrord", async function() {
        console.log("Enabling mirrord");        
        const statusBar = new StatusBar();
        const enableButton = await statusBar.getItem("Enable mirrord");
        assert(enableButton !== undefined, "Enable mirrord button not found");
        await enableButton?.click();                
        assert(await enableButton?.getText() == "Disable mirrord", "Disable mirrord button not found");
    });

    it("create mirrord config", async function() {
        console.log("Clicking on the create mirrord settings button");        
        const statusBar = new StatusBar();
        const items = await statusBar.getItems();
        for (const item of items) {
            const text = await item.getText();            
            console.log(text);
        }
    });
});

async function sleep(time: number) {
    await new Promise((resolve) => setTimeout(resolve, time));
}