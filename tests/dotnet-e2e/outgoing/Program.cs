using Flurl.Http;

var response = await "http://www.google.com/robots.txt".GetAsync();

if (response.StatusCode > 299)
    throw new Exception("Response failed with status code");

Console.WriteLine(await response.GetStringAsync());
