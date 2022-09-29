// https://learn.microsoft.com/en-us/aspnet/core/fundamentals/servers/kestrel?view=aspnetcore-6.0#get-started
// "the WebApplication.CreateBuilder method calls UseKestrel internally"
var builder = WebApplication.CreateBuilder(args);
builder.WebHost.UseUrls("http://0.0.0.0:80");

var app = builder.Build();

app.MapGet("/", () => "GET");

app.MapPost("/", () => "POST");

app.MapPut("/", () => "PUT");

app.MapDelete("/", () => "DELETE");

Console.WriteLine("Kestrel server starting");
app.Run();
