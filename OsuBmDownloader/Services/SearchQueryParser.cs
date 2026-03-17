using System.Text.RegularExpressions;
using OsuBmDownloader.Models;

namespace OsuBmDownloader.Services;

public class SearchQueryParser
{
    public string TextQuery { get; private set; } = string.Empty;
    public List<FilterCondition> Filters { get; } = new();

    // Matches patterns like: star>=5, bpm<=200, ar>9, length<120, cs=4
    // Allows optional spaces around the operator
    private static readonly Regex FilterPattern = new(
        @"(star|bpm|length|ar|cs|od|hp)\s*(>=|<=|>|<|=)\s*(\d+(?:\.\d+)?)",
        RegexOptions.IgnoreCase | RegexOptions.Compiled);

    public static SearchQueryParser Parse(string input)
    {
        var parser = new SearchQueryParser();
        if (string.IsNullOrWhiteSpace(input))
            return parser;

        // Extract all filter conditions
        var cleaned = FilterPattern.Replace(input, match =>
        {
            var field = match.Groups[1].Value.ToLower();
            var op = match.Groups[2].Value;
            var value = double.Parse(match.Groups[3].Value);

            parser.Filters.Add(new FilterCondition(field, op, value));
            return ""; // remove from text query
        });

        // Remove '&' separators and extra whitespace
        cleaned = cleaned.Replace("&", " ");
        cleaned = Regex.Replace(cleaned, @"\s+", " ").Trim();

        parser.TextQuery = cleaned;
        return parser;
    }

    /// <summary>
    /// Tests whether a beatmap set passes all filter conditions.
    /// A set passes if ANY of its difficulties satisfies all conditions
    /// (so "star>=5 & ar>=9" means at least one diff has both star>=5 AND ar>=9).
    /// For set-level range filters (star, bpm, length), we check if the set's
    /// range overlaps the condition (i.e., has any diff that could match).
    /// </summary>
    public bool Matches(BeatmapSet set)
    {
        if (Filters.Count == 0) return true;

        // Check per-difficulty: at least one diff must pass all filters
        if (set.Beatmaps.Count == 0) return false;

        return set.Beatmaps.Any(diff =>
        {
            foreach (var f in Filters)
            {
                var val = GetDiffValue(diff, f.Field);
                if (!f.Evaluate(val)) return false;
            }
            return true;
        });
    }

    private static double GetDiffValue(Beatmap diff, string field) => field switch
    {
        "star" => diff.DifficultyRating,
        "bpm" => diff.Bpm,
        "length" => diff.TotalLength,
        "ar" => diff.AR,
        "cs" => diff.CS,
        "od" => diff.OD,
        "hp" => diff.HP,
        _ => 0
    };
}

public class FilterCondition
{
    public string Field { get; }
    public string Operator { get; }
    public double Value { get; }

    public FilterCondition(string field, string op, double value)
    {
        Field = field;
        Operator = op;
        Value = value;
    }

    public bool Evaluate(double actual) => Operator switch
    {
        ">=" => actual >= Value,
        "<=" => actual <= Value,
        ">" => actual > Value,
        "<" => actual < Value,
        "=" => Math.Abs(actual - Value) < 0.01,
        _ => true
    };
}
