const portfinder = require('portfinder')

portfinder.getPortPromise().then((number) => {
    console.log(`Found port ${number}`);
})
