var TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";

// Read

{
    var dat = File.ReadAllText("/app/test.txt");

    if (dat != TEXT)
        throw new Exception($"Expected {TEXT}, got {dat}");
}


// Write

{
    var filePath = createTempFile();
    checkFileExistsOnHost(filePath);

    var dat = File.ReadAllText(filePath);

    if (dat != TEXT)
        throw new Exception($"Expected {TEXT}, got {dat}");

    File.Delete(filePath);
}


// Lseek

{
    var filePath = createTempFile();
    checkFileExistsOnHost(filePath);

    var fileStream = File.Open(filePath, FileMode.Open);
    var buf = new byte[TEXT.Length];

    // https://learn.microsoft.com/en-us/dotnet/api/system.io.filestream.canseek?view=net-6.0
    if (!fileStream.CanSeek)
        throw new Exception("file stream CanSeek failure");

    // Arg 2 specifies offset for stream
    fileStream.Read(buf, 0, buf.Length);

    if (System.Text.Encoding.Default.GetString(buf) != TEXT)
        throw new Exception($"Expected {TEXT}, got {System.Text.Encoding.Default.GetString(buf)}");

    fileStream.Close();
    File.Delete(filePath);
}


// Faccessat
// TODO: syscall limitations x-platform. Will revisit after discussion

{
    var filePath = createTempFile();
    checkFileExistsOnHost(filePath);

    var fileStream = File.Open(filePath, FileMode.Append);

    fileStream.Close();
    File.Delete(filePath);
}

// helpers

string createTempFile()
{
    var filePath = $"/tmp/tmpF-{Guid.NewGuid()}.txt";
    File.WriteAllText(filePath, TEXT);
    return filePath;
}

void checkFileExistsOnHost(string fileName)
{
    if (File.Exists(fileName))
        throw new Exception("file exists on host");
}
