import { createSocket } from 'dgram';

const socket = createSocket('udp4');

const payload = Uint8Array.from(Buffer.from("076d6972726f7264036465760000010001", "hex"));

socket.send(payload, 0, payload.length, 53, '8.8.8.8', (err, bytes) => {
  if (err) {
    console.error(err);
    process.exit(1);
  } else {
    console.log(`Sent ${bytes} bytes to 8.8.8.8`);
  }
});

socket.on('message', (msg, _) => {  
  console.log(msg);
  process.exit(0);
});

socket.on('error', (err) => {
    process.exit(1);
});
