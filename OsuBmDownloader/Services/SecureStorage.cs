using System.IO;
using System.Security.Cryptography;
using System.Text;

namespace OsuBmDownloader.Services;

/// <summary>
/// Encrypts/decrypts data using Windows DPAPI (tied to current user account).
/// Files are unreadable outside this user session.
/// </summary>
public static class SecureStorage
{
    public static void WriteEncrypted(string filePath, string json)
    {
        var plainBytes = Encoding.UTF8.GetBytes(json);
        var encrypted = ProtectedData.Protect(plainBytes, null, DataProtectionScope.CurrentUser);
        File.WriteAllBytes(filePath, encrypted);
    }

    public static string? ReadEncrypted(string filePath)
    {
        if (!File.Exists(filePath))
            return null;

        try
        {
            var encrypted = File.ReadAllBytes(filePath);
            var plainBytes = ProtectedData.Unprotect(encrypted, null, DataProtectionScope.CurrentUser);
            return Encoding.UTF8.GetString(plainBytes);
        }
        catch
        {
            // File corrupted or tampered with — treat as missing
            return null;
        }
    }
}
