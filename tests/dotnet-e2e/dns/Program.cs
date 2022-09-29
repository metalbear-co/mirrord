using System.Net;

var url = "https://google.com";
Uri myUri = new Uri(url);
var ip = (await Dns.GetHostAddressesAsync(myUri.Host))[0];
Console.WriteLine(ip);
