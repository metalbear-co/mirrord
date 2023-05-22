import { existsSync, stat } from "fs";
import { assert }  from "chai"
import { join } from "path";
import { By, VSBrowser, EditorView, WebView, Workbench, Notification, StatusBar, NotificationType } from "vscode-extension-tester";


describe("mirrord sample flow test", function() {
    console.log("Initializing workspace");
    this.timeout(10000000000);
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
        this.timeout(1000000);
        console.log("Clicking on the create mirrord settings button");        
        const statusBar = new StatusBar();

        const items = await statusBar.getItems();
        for (const item of items) {
            console.log(await item.getId());
            console.log(await item.getAttribute("value"));            
        }
        // const mirrordSettingsButton = await statusBar.getItem("mirrord.changeSettings"); 
        // console.log("Mirrord settings button: " + mirrordSettingsButton);   
        // assert(mirrordSettingsButton != undefined, "Mirrord settings button not found");    
        // await mirrordSettingsButton?.click();        
        // await browser.driver.wait(async () => {            
        //     const mirrordConfigPath = join(__dirname, '../../test-workspace/.mirrord/mirrord.json');
        //     return await existsSync(mirrordConfigPath);
        // }
        // , 100000, "Mirrord config not found");
    });
});


async function sleep(time: number) {
    await new Promise((resolve) => setTimeout(resolve, time));
}