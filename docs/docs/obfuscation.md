---
sidebar_position: 5
---

# Obfuscation Policies

Obfuscation is an important feature in Postie that helps protect your posts from automated takedown systems and provides privacy. This guide explains the different obfuscation policies available.

## Why Use Obfuscation?

Obfuscation serves several purposes:

1. **Privacy Protection**: Masks the identity of the poster
2. **Content Protection**: Makes it harder for automated systems to identify and remove content
3. **DMCA Avoidance**: Helps avoid automated DMCA takedown notices
4. **Longevity**: Increases the lifespan of your posts on Usenet

## Available Obfuscation Policies

Postie has three policies. `obfuscation_policy` applies to your files and `par2_obfuscation_policy` to the
PAR2 recovery files.

| | `full` | `partial` | `none` |
|---|---|---|---|
| **Subject** | Random text, new for every article | MD5 of the normal subject, different per article | The real file name and part number |
| **yEnc file name** | Random text, new for every article | MD5 of file name plus part number, different per article | The real file name |
| **Date** | Random time within the last 6 hours | Real posting time | Real posting time |
| **Poster (From)** | Random, new for every article | One random poster for the whole file | One random poster for the whole file |
| **X-NXG header** | Never | When `add_nxg_header` is on | When `add_nxg_header` is on |

A few rules apply across policies:

- **`default_from` wins.** When `posting.post_headers.default_from` is set, every article uses it as the
  poster, whatever the policy.
- **PAR2 files follow the main policy for date, poster and X-NXG.** `par2_obfuscation_policy` only changes
  their subjects and yEnc file names.
- **The NZB always has the real names.** Obfuscation hides names on the servers; the NZB maps the
  articles back to the original files, so downloaders get them with their real names.
- **`message_id_format: nxg`** builds deterministic message IDs and, with `partial`, NXG-style
  obfuscated subjects. The X-NXG header is never added with `nxg` message IDs.

```yaml
posting:
  obfuscation_policy: full
  par2_obfuscation_policy: full
  post_headers:
    default_from: "" # leave empty to keep random posters
```

## Separate Policies for Regular Files and PAR2 Files

```yaml
posting:
  obfuscation_policy: full # your files
  par2_obfuscation_policy: none # PAR2 files keep readable subjects
```

This makes PAR2 files easier to find by subject while the main files stay obfuscated.

## Best Practices

1. **Full Obfuscation for Sensitive Content**: Use full obfuscation for content that might be subject to takedown notices.
2. **Partial Obfuscation for General Use**: A good balance between protection and usability.
3. **No Obfuscation for Public Content**: Only use when posting public domain or your own original content.
4. **Mixed Strategy**: Consider using different obfuscation levels for main content vs. PAR2 files to balance discoverability with protection.
