// SPDX-License-Identifier: GPL-2.0-only
// Native libclang AST dependency extractor for DRY OS H2 Driver Island analysis.

#include <clang-c/Index.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct ExtractCtx {
    const char *src_dir;
};

static void print_json_escaped(const char *s) {
    putchar('"');
    if (s) {
        for (const unsigned char *p = (const unsigned char *)s; *p; ++p) {
            if (*p == '"' || *p == '\\') {
                putchar('\\');
                putchar(*p);
            } else if (*p == '\n') {
                fputs("\\n", stdout);
            } else if (*p >= 32) {
                putchar(*p);
            }
        }
    }
    putchar('"');
}

static int is_in_driver_dir(CXSourceLocation loc, const char *src_dir) {
    CXFile file = NULL;
    clang_getExpansionLocation(loc, &file, NULL, NULL, NULL);
    if (!file) {
        return 0;
    }
    CXString fname = clang_getFileName(file);
    const char *cstr = clang_getCString(fname);
    int match = (cstr && strncmp(cstr, src_dir, strlen(src_dir)) == 0);
    clang_disposeString(fname);
    return match;
}

static enum CXChildVisitResult visitor(CXCursor cursor, CXCursor parent, CXClientData client_data) {
    (void)parent;
    struct ExtractCtx *ctx = (struct ExtractCtx *)client_data;
    CXSourceLocation loc = clang_getCursorLocation(cursor);

    if (!is_in_driver_dir(loc, ctx->src_dir)) {
        return CXChildVisit_Continue;
    }

    enum CXCursorKind kind = clang_getCursorKind(cursor);

    if (kind == CXCursor_CallExpr) {
        CXCursor ref = clang_getCursorReferenced(cursor);
        if (!clang_Cursor_isNull(ref) && clang_getCursorKind(ref) == CXCursor_FunctionDecl) {
            CXSourceLocation decl_loc = clang_getCursorLocation(ref);
            if (!is_in_driver_dir(decl_loc, ctx->src_dir)) {
                CXFile decl_file = NULL;
                clang_getExpansionLocation(decl_loc, &decl_file, NULL, NULL, NULL);
                CXString fn_name = clang_getCursorSpelling(ref);
                CXString fn_type = clang_getTypeSpelling(clang_getCursorType(ref));
                int inlined = clang_Cursor_isFunctionInlined(ref);

                fputs("{\"kind\":\"ext_call\",\"name\":", stdout);
                print_json_escaped(clang_getCString(fn_name));
                fputs(",\"sig\":", stdout);
                print_json_escaped(clang_getCString(fn_type));
                fputs(",\"header\":", stdout);
                if (decl_file) {
                    CXString hdr_name = clang_getFileName(decl_file);
                    print_json_escaped(clang_getCString(hdr_name));
                    clang_disposeString(hdr_name);
                } else {
                    print_json_escaped("");
                }
                fprintf(stdout, ",\"inlined\":%s}\n", inlined ? "true" : "false");

                clang_disposeString(fn_name);
                clang_disposeString(fn_type);
            }
        }
    } else if (kind == CXCursor_MemberRefExpr) {
        CXCursor ref = clang_getCursorReferenced(cursor);
        if (!clang_Cursor_isNull(ref) && clang_getCursorKind(ref) == CXCursor_FieldDecl) {
            CXCursor sem_parent = clang_getCursorSemanticParent(ref);
            CXSourceLocation rec_loc = clang_getCursorLocation(sem_parent);
            if (!is_in_driver_dir(rec_loc, ctx->src_dir)) {
                CXString rec_name = clang_getTypeSpelling(clang_getCanonicalType(clang_getCursorType(sem_parent)));
                CXString fld_name = clang_getCursorSpelling(ref);
                const char *r_cstr = clang_getCString(rec_name);
                if (r_cstr && r_cstr[0] != '\0' && strstr(r_cstr, "unnamed") == NULL) {
                    fputs("{\"kind\":\"ext_field\",\"record\":", stdout);
                    print_json_escaped(r_cstr);
                    fputs(",\"field\":", stdout);
                    print_json_escaped(clang_getCString(fld_name));
                    fputs("}\n", stdout);
                }
                clang_disposeString(rec_name);
                clang_disposeString(fld_name);
            }
        }
    } else if (kind == CXCursor_VarDecl) {
        CXType vtype = clang_getCanonicalType(clang_getCursorType(cursor));
        CXString tspell = clang_getTypeSpelling(vtype);
        const char *ts = clang_getCString(tspell);
        if (ts && (strstr(ts, "pci_driver") || strstr(ts, "net_device_ops") ||
                   strstr(ts, "ethtool_ops") || strstr(ts, "dev_pm_ops"))) {
            CXString vname = clang_getCursorSpelling(cursor);
            fputs("{\"kind\":\"ops_table\",\"var\":", stdout);
            print_json_escaped(clang_getCString(vname));
            fputs(",\"type\":", stdout);
            print_json_escaped(ts);
            fputs("}\n", stdout);
            clang_disposeString(vname);
        }
        clang_disposeString(tspell);
    }

    return CXChildVisit_Recurse;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "Usage: %s <driver_src_dir> <tu.c> [clang_args...]\n", argv[0]);
        return 2;
    }
    const char *src_dir = argv[1];
    const char *tu_path = argv[2];
    const char * const *clang_args = (const char * const *)(argv + 3);
    int num_args = argc - 3;

    CXIndex idx = clang_createIndex(0, 0);
    CXTranslationUnit tu = NULL;
    enum CXErrorCode err = clang_parseTranslationUnit2(
        idx,
        tu_path,
        clang_args,
        num_args,
        NULL,
        0,
        CXTranslationUnit_KeepGoing,
        &tu
    );
    if (err != CXError_Success || !tu) {
        fprintf(stderr, "libclang failed to parse %s (err=%d)\n", tu_path, (int)err);
        clang_disposeIndex(idx);
        return 1;
    }

    unsigned ndiag = clang_getNumDiagnostics(tu);
    unsigned fatal_errors = 0;
    for (unsigned i = 0; i < ndiag; ++i) {
        CXDiagnostic d = clang_getDiagnostic(tu, i);
        enum CXDiagnosticSeverity sev = clang_getDiagnosticSeverity(d);
        if (sev >= CXDiagnostic_Error) {
            fatal_errors++;
            CXString s = clang_formatDiagnostic(d, clang_defaultDiagnosticDisplayOptions());
            fprintf(stderr, "[AST-DIAG] %s\n", clang_getCString(s));
            clang_disposeString(s);
        }
        clang_disposeDiagnostic(d);
    }
    fprintf(stdout, "{\"kind\":\"tu_summary\",\"tu\":");
    print_json_escaped(tu_path);
    fprintf(stdout, ",\"errors\":%u}\n", fatal_errors);

    struct ExtractCtx ctx = { .src_dir = src_dir };
    clang_visitChildren(clang_getTranslationUnitCursor(tu), visitor, &ctx);

    clang_disposeTranslationUnit(tu);
    clang_disposeIndex(idx);
    return 0;
}
