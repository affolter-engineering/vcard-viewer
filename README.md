# vcard-viewer

A command-line tool that parses and displays vCards in a human-readable
format. Supports vCard 2.1, 3.0 and 4.0.

## Features

- Display individual vCard files with colorized and structured output
- Display all `.vcf`/`.vcard` files in a directory as a summary table
- Export contact data as RFC 4180 CSV
- Handles quoted-printable encoding, vCard text escapes and Mojibake repair
- Supports multi-value fields (multiple emails, phones, addresses, URLs, etc.)

## Installation

### From source

```sh
$ cargo install --path .
```

### Nix

```sh
$ nix run .
```

## Usage

```
$ vcard-viewer [--csv] <file.vcf | directory>
```

### Single file

Displays each contact in the file with a labeled, colorized card layout:

```sh
$ vcard-viewer contact.vcf
```

### Directory

Displays all `.vcf`/`.vcard` files in the directory as a table:

```sh
$ vcard-viewer ~/contacts/
```

### CSV export

Outputs all contacts as CSV to stdout:

```sh
$ vcard-viewer --csv contact.vcf > contacts.csv
$ vcard-viewer --csv ~/contacts/ > contacts.csv
```

## Output

**Single file** - one card per contact showing name, organisation, title/role, emails, phones, addresses, birthday, anniversary, websites, IMs, and notes.

**Directory** - summary table with columns: `#`, `Name`, `Organisation`, `Title/Role`, `Email`, `Phone`.

**CSV** - columns: `source`, `name`, `organisation`, `title`, `role`, `email`, `phone`, `address`, `birthday`, `anniversary`, `website`, `note`. Multiple values within a field are separated by ` | `.

## License

MIT
