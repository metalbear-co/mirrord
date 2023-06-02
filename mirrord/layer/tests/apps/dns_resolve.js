const dns = require('dns');

const hostname = 'example.com';

dns.resolve(hostname, (err, addresses) => {
  if (err) {
    console.error(err);
    return;
  }

  console.log(`IP addresses for ${hostname}:`);
  addresses.forEach((address) => {
    console.log(address);
  });
});