using System.Net;

Uri uri = new(uriString: "https://google.com");
await Dns.GetHostAddressesAsync(uri.Host);
