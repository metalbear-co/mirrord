import https from "node:https";
import { exit } from "node:process";

throw "Failure";
// process.exit(-1);

// function main() {
//   return new Promise((resolve, reject) => {
//     const options = {
//       hostname: "www.bonkers1234.com.net",
//       port: 443,
//       path: "/",
//       method: "GET",
//     };

//     const request = https.request(options, (response) => {
//       console.log(`statusCode: ${response.statusCode}`);

//       response.on("data", (data) => {
//         process.stdout.write(data);
//       });

//       response.on("error", (fail) => {
//         process.stderr.write(`>> response failed with ${fail}`);
//         throw fail;
//       });

//       response.on("end", () => {
//         resolve(null);
//       });

//       if (response.statusCode !== 200) {
//         throw ">> response.statusCode !== 200";
//       }
//     });

//     request.on("error", (fail) => {
//       process.stderr.write(`>> request failed with ${fail}`);
//       throw fail;
//     });

//     request.end();
//   });
// }

// main()
//   .then((value) => {
//     process.exit();
//   })
//   .catch((fail) => {
//     process.stderr.write(`>> failed with ${fail}`);
//     process.exit(-1);
//   });
