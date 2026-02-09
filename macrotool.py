#!/usr/bin/env python3
"""CLI tool for managing recorded macros.

Usage:
    python3 macrotool.py list
    python3 macrotool.py info <id>
    python3 macrotool.py rename <id> <new_name>
    python3 macrotool.py delete <id>
    python3 macrotool.py export <id> <output_path>
"""
import argparse
import shutil
import sys
from pathlib import Path

from macro import (
    MACROS_DIR,
    delete_macro,
    get_macro_info,
    list_macros,
    rename_macro,
)


def cmd_list(args):
    macros = list_macros()
    if not macros:
        print("No macros recorded.")
        return
    print(f"{'Slot':>4}  {'ID':>4}  {'Name':<24}  {'Frames':>7}  {'Duration':>10}  Created")
    print("-" * 80)
    for i, m in enumerate(macros):
        dur = m.get("duration_ms", 0)
        dur_str = f"{dur // 1000}.{dur % 1000:03d}s" if dur >= 1000 else f"{dur}ms"
        print(f"{i:>4}  {m['id']:>4}  {m['name']:<24}  {m['frame_count']:>7}  {dur_str:>10}  {m.get('created', 'N/A')}")


def cmd_info(args):
    info = get_macro_info(args.id)
    if info is None:
        print(f"Macro {args.id} not found.", file=sys.stderr)
        sys.exit(1)

    filepath = MACROS_DIR / info["filename"]
    file_size = filepath.stat().st_size if filepath.exists() else 0

    print(f"Macro #{info['id']}")
    print(f"  Name:       {info['name']}")
    print(f"  File:       {info['filename']}")
    print(f"  Frames:     {info['frame_count']}")
    print(f"  Duration:   {info.get('duration_ms', 0)}ms")
    print(f"  File size:  {file_size:,} bytes")
    print(f"  Created:    {info.get('created', 'N/A')}")


def cmd_rename(args):
    if rename_macro(args.id, args.new_name):
        print(f"Macro {args.id} renamed to '{args.new_name}'.")
    else:
        print(f"Macro {args.id} not found.", file=sys.stderr)
        sys.exit(1)


def cmd_delete(args):
    info = get_macro_info(args.id)
    if info is None:
        print(f"Macro {args.id} not found.", file=sys.stderr)
        sys.exit(1)

    if not args.force:
        confirm = input(f"Delete macro '{info['name']}' (ID {args.id})? [y/N] ")
        if confirm.lower() != "y":
            print("Cancelled.")
            return

    if delete_macro(args.id):
        print(f"Macro {args.id} deleted.")
    else:
        print(f"Failed to delete macro {args.id}.", file=sys.stderr)
        sys.exit(1)


def cmd_export(args):
    info = get_macro_info(args.id)
    if info is None:
        print(f"Macro {args.id} not found.", file=sys.stderr)
        sys.exit(1)

    src = MACROS_DIR / info["filename"]
    if not src.exists():
        print(f"Macro file not found: {src}", file=sys.stderr)
        sys.exit(1)

    dst = Path(args.output)
    shutil.copy2(src, dst)
    print(f"Exported macro {args.id} to {dst}")


def main():
    parser = argparse.ArgumentParser(
        description="Manage recorded MITM macros."
    )
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("list", help="List all recorded macros")

    p_info = sub.add_parser("info", help="Show details for a macro")
    p_info.add_argument("id", type=int, help="Macro ID")

    p_rename = sub.add_parser("rename", help="Rename a macro")
    p_rename.add_argument("id", type=int, help="Macro ID")
    p_rename.add_argument("new_name", help="New name for the macro")

    p_delete = sub.add_parser("delete", help="Delete a macro")
    p_delete.add_argument("id", type=int, help="Macro ID")
    p_delete.add_argument("-f", "--force", action="store_true", help="Skip confirmation")

    p_export = sub.add_parser("export", help="Export a macro .bin file")
    p_export.add_argument("id", type=int, help="Macro ID")
    p_export.add_argument("output", help="Output file path")

    args = parser.parse_args()
    {
        "list": cmd_list,
        "info": cmd_info,
        "rename": cmd_rename,
        "delete": cmd_delete,
        "export": cmd_export,
    }[args.command](args)


if __name__ == "__main__":
    main()
