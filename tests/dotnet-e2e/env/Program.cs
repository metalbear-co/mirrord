Dictionary<string, string> environmentVariables = new()
{
    ["MIRRORD_FAKE_VAR_FIRST"] = "mirrord.is.running",
    ["MIRRORD_FAKE_VAR_SECOND"] = "7777",
    ["MIRRORD_FAKE_VAR_THIRD"] = "foo=bar",
};

foreach (var (key, value) in environmentVariables)
{
    if (Environment.GetEnvironmentVariable(key, EnvironmentVariableTarget.Process) != value)
        throw new Exception($"missing env var {key}");
}
