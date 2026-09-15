#!/usr/bin/env python3
"""Generate invariant scalar mappings using .NET's native ICU casing routine.

Usage: python generate_dotnet_casing.py /path/to/libSystem.Globalization.Native.so
Requires the Linux .NET runtime and its ICU dependency, but no SDK. This calls
GlobalizationNative_ChangeCaseInvariant, the native implementation used by
String.ToUpperInvariant/ToLowerInvariant with ICU globalization enabled.
The fixture records only nonidentity rows; all omitted scalars map to themselves.
"""
import ctypes
import pathlib
import sys

library = pathlib.Path(sys.argv[1]).resolve()
runtime = ctypes.CDLL(str(library))
assert runtime.GlobalizationNative_LoadICU() == 1, "Unable to initialize ICU"
u16 = ctypes.c_uint16
change = runtime.GlobalizationNative_ChangeCaseInvariant
change.argtypes = [ctypes.POINTER(u16), ctypes.c_int32, ctypes.POINTER(u16), ctypes.c_int32, ctypes.c_int32]
change.restype = None
output = pathlib.Path(__file__).with_name("dotnet-invariant-casing.tsv")
with output.open("w") as f:
    f.write(f"# Generated from .NET {library.parent.name} ChangeCaseInvariant; ICU version 0x{runtime.GlobalizationNative_GetICUVersion():08X}\n")
    f.write("# scalar uppercase lowercase (hex); omitted scalars are identity mappings\n")
    for code in range(0x110000):
        if 0xD800 <= code <= 0xDFFF:
            continue
        raw = chr(code).encode("utf-16-le")
        length = len(raw) // 2
        source = (u16 * length).from_buffer_copy(raw)
        values = []
        for upper in (1, 0):
            dest = (u16 * length)()
            change(source, length, dest, length, upper)
            mapped = bytes(dest).decode("utf-16-le")
            assert len(mapped) == 1
            values.append(ord(mapped))
        if values != [code, code]:
            f.write(f"{code:06X}\t{values[0]:06X}\t{values[1]:06X}\n")
print(output)
