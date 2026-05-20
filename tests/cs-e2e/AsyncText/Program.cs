// E2E test: read /app/test.txt asynchronously through .NET's
// `File.ReadAllTextAsync`, which on Windows uses overlapped IO + IO
// completion ports under the hood -- exercises the same syscall path
// (CreateFile + FileCompletionInformation bind + NtReadFile returning
// STATUS_PENDING + completion packet on the runtime's threadpool IOCP)
// that any real .NET app hits when reading a file asynchronously.
//
// The test calls `ReadAllTextAsync` and ONLY `ReadAllTextAsync` -- no
// preceding sync call, no FileStream warmup -- so the async IOCP path
// is exercised from a clean process state. The result is compared
// byte-for-byte against a vendored copy of the Lorem Ipsum that the
// `mirrord-pytest:latest` image ships at /app/test.txt (cited at
// `tests/python-e2e/files_ro.py:7` and `tests/src/utils.rs:27`).
//
// Exits non-zero on any mismatch; the Rust driver in
// `tests/src/iocp.rs` asserts on the exit code.
using System;
using System.IO;
using System.Text;
using System.Threading.Tasks;

internal class Program
{
    // Vendored expected content. The agent serves the same bytes
    // back; we don't need a runtime fetch.
    private const string ExpectedLoremIpsum =
        "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do " +
        "eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim " +
        "ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut " +
        "aliquip ex ea commodo consequat. Duis aute irure dolor in " +
        "reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla " +
        "pariatur. Excepteur sint occaecat cupidatat non proident, sunt in " +
        "culpa qui officia deserunt mollit anim id est laborum.\n";

    private static async Task<int> Main()
    {
        // .NET on Windows accepts `/` as a separator and resolves
        // `/app/test.txt` relative to the current drive, so this
        // becomes the NT path `\??\<drive>:\app\test.txt` the layer
        // sees and translates back to `/app/test.txt` for the agent.
        const string path = "/app/test.txt";

        Console.WriteLine($"[{Stamp()}] await File.ReadAllTextAsync(\"{path}\")");
        string content;
        try
        {
            content = await File.ReadAllTextAsync(path);
        }
        catch (Exception e)
        {
            Console.Error.WriteLine($"FAIL: ReadAllTextAsync threw {e.GetType().Name}: {e.Message}");
            return 1;
        }
        Console.WriteLine(
            $"[{Stamp()}] got {content.Length} chars, first 32: " +
            $"{EscapeForLog(content[..Math.Min(32, content.Length)])}");

        if (content.Length != ExpectedLoremIpsum.Length)
        {
            Console.Error.WriteLine(
                $"FAIL: length mismatch -- got {content.Length}, expected {ExpectedLoremIpsum.Length}");
            return 2;
        }

        if (content != ExpectedLoremIpsum)
        {
            // First-mismatch diagnostic.
            int diff = 0;
            while (diff < content.Length && content[diff] == ExpectedLoremIpsum[diff])
            {
                diff++;
            }
            Console.Error.WriteLine(
                $"FAIL: content mismatch at offset {diff}: " +
                $"got '{EscapeForLog(content[diff].ToString())}' " +
                $"(0x{(int)content[diff]:X2}), expected " +
                $"'{EscapeForLog(ExpectedLoremIpsum[diff].ToString())}' " +
                $"(0x{(int)ExpectedLoremIpsum[diff]:X2})");
            return 3;
        }

        Console.WriteLine("OK: ReadAllTextAsync content matches expected Lorem Ipsum");
        return 0;
    }

    private static string Stamp() => DateTime.UtcNow.ToString("HH:mm:ss.fff");

    private static string EscapeForLog(string s)
    {
        var sb = new StringBuilder();
        foreach (var c in s)
        {
            sb.Append(c switch
            {
                '\n' => "\\n",
                '\r' => "\\r",
                '\t' => "\\t",
                _ when c < 32 || c > 126 => $"\\x{(int)c:X2}",
                _ => c.ToString(),
            });
        }
        return sb.ToString();
    }
}
