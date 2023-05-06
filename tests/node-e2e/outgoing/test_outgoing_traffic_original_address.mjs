import { resolve } from 'dns';

const hostname = 'example.com';
const qtype = 'A';

resolve(hostname, qtype, (err, addresses) => {
  if (err) {
    console.error(err);    
  } else {
    console.log(`IP addresses for ${hostname}: ${addresses}`);
  }
});