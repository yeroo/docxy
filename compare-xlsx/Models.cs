using System.Text.Json.Serialization;

namespace XlsxyCompare;

/// <summary>The corpus/classification-xlsx.json manifest.</summary>
public sealed class Manifest
{
    [JsonPropertyName("root")] public string Root { get; set; } = "xlsx";
    [JsonPropertyName("count")] public int Count { get; set; }
    [JsonPropertyName("categories")] public Dictionary<string, int> Categories { get; set; } = new();
    [JsonPropertyName("tags")] public Dictionary<string, TagInfo> Tags { get; set; } = new();
    [JsonPropertyName("files")] public List<FileEntry> Files { get; set; } = new();
}

public sealed class TagInfo
{
    [JsonPropertyName("count")] public int Count { get; set; }
    [JsonPropertyName("doc")] public string Doc { get; set; } = "";
}

public sealed class FileEntry
{
    [JsonPropertyName("name")] public string Name { get; set; } = "";
    [JsonPropertyName("path")] public string Path { get; set; } = "";
    [JsonPropertyName("folder")] public string Folder { get; set; } = "";
    [JsonPropertyName("category")] public string Category { get; set; } = "";
    [JsonPropertyName("tags")] public List<string> Tags { get; set; } = new();
    [JsonPropertyName("functions")] public List<string> Functions { get; set; } = new();
    [JsonPropertyName("size")] public long Size { get; set; }
}
