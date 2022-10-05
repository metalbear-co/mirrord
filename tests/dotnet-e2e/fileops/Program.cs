var TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";
TestRead();
TestWrite();
TestLseek();

void TestRead()
{
    var dat = File.ReadAllText("/app/test.txt");

    if (dat != TEXT)
        throw new Exception($"Expected {TEXT}, got {dat}");
}

void TestWrite()
{
    var filePath = createTempFile();

    var dat = File.ReadAllText(filePath);

    if (dat != TEXT)
        throw new Exception($"Expected {TEXT}, got {dat}");

    File.Delete(filePath);
}

void TestLseek()
{
    var filePath = createTempFile();

    var fileStream = File.Open(filePath, FileMode.Open);
    var buf = new byte[TEXT.Length];

    // Arg 2 specifies offset for stream
    fileStream.Read(buf, 0, buf.Length);

    var decodedBuffer = System.Text.Encoding.Default.GetString(buf);

    if (decodedBuffer != TEXT)
        throw new Exception($"Expected {TEXT}, got {decodedBuffer}");

    fileStream.Close();
    File.Delete(filePath);
}

string createTempFile()
{
    var filePath = $"/tmp/tmpF-{Guid.NewGuid()}.txt";
    File.WriteAllText(filePath, TEXT);
    return filePath;
}
