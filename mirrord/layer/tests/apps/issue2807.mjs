import { getPortPromise } from "portfinder";

getPortPromise().then((number) => {
    console.log(`Found port ${number}`);
})
