; ModuleID = '/tmp/claude-501/facts-guard.c'
source_filename = "/tmp/claude-501/facts-guard.c"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx26.0.0"

@table = local_unnamed_addr constant [2 x ptr] [ptr @one, ptr @two], align 8, !dbg !0

; Function Attrs: mustprogress nofree norecurse nosync nounwind ssp willreturn memory(none) uwtable(sync)
define internal noundef i32 @one(i32 noundef returned %0) #0 !dbg !26 {
    #dbg_value(i32 %0, !28, !DIExpression(), !29)
  ret i32 %0, !dbg !30
}

; Function Attrs: mustprogress nofree norecurse nosync nounwind ssp willreturn memory(none) uwtable(sync)
define internal range(i32 -2147483647, -2147483648) i32 @two(i32 noundef %0) #0 !dbg !31 {
    #dbg_value(i32 %0, !33, !DIExpression(), !34)
  %2 = add nsw i32 %0, 1, !dbg !35
  ret i32 %2, !dbg !36
}

; Function Attrs: nounwind ssp uwtable(sync)
define i32 @dispatch(i32 noundef %0, i32 noundef %1) local_unnamed_addr #1 !dbg !37 {
    #dbg_value(i32 %0, !41, !DIExpression(), !43)
    #dbg_value(i32 %1, !42, !DIExpression(), !43)
  %3 = sext i32 %0 to i64, !dbg !44
  %4 = getelementptr inbounds [8 x i8], ptr @table, i64 %3, !dbg !44
  %5 = load ptr, ptr %4, align 8, !dbg !44, !tbaa !45
  %6 = tail call i32 %5(i32 noundef %1) #3, !dbg !47
  ret i32 %6, !dbg !48
}

; Function Attrs: nounwind ssp uwtable(sync)
define range(i32 -2147483647, -2147483648) i32 @main() local_unnamed_addr #1 !dbg !49 {
    #dbg_value(i32 2, !52, !DIExpression(), !56)
    #dbg_value(i32 3, !55, !DIExpression(), !56)
  %1 = tail call i32 @outer(i32 noundef 5) #3, !dbg !58
  %2 = add nsw i32 %1, 1, !dbg !59
  ret i32 %2, !dbg !60
}

declare !dbg !61 i32 @outer(i32 noundef) local_unnamed_addr #2

@dispatch_alias = alias i32 (i32, i32), ptr @dispatch

attributes #0 = { mustprogress nofree norecurse nosync nounwind ssp willreturn memory(none) uwtable(sync) "frame-pointer"="non-leaf-no-reserve" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "target-cpu"="apple-m1" "target-features"="+aes,+altnzcv,+ccdp,+ccidx,+ccpp,+complxnum,+crc,+dit,+dotprod,+flagm,+fp-armv8,+fp16fml,+fptoint,+fullfp16,+jsconv,+lse,+neon,+pauth,+perfmon,+predres,+ras,+rcpc,+rdm,+sb,+sha2,+sha3,+specrestrict,+ssbs,+v8.1a,+v8.2a,+v8.3a,+v8.4a,+v8a" "tune-cpu"="apple-m5" }
attributes #1 = { nounwind ssp uwtable(sync) "frame-pointer"="non-leaf-no-reserve" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "target-cpu"="apple-m1" "target-features"="+aes,+altnzcv,+ccdp,+ccidx,+ccpp,+complxnum,+crc,+dit,+dotprod,+flagm,+fp-armv8,+fp16fml,+fptoint,+fullfp16,+jsconv,+lse,+neon,+pauth,+perfmon,+predres,+ras,+rcpc,+rdm,+sb,+sha2,+sha3,+specrestrict,+ssbs,+v8.1a,+v8.2a,+v8.3a,+v8.4a,+v8a" "tune-cpu"="apple-m5" }
attributes #2 = { "frame-pointer"="non-leaf-no-reserve" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "target-cpu"="apple-m1" "target-features"="+aes,+altnzcv,+ccdp,+ccidx,+ccpp,+complxnum,+crc,+dit,+dotprod,+flagm,+fp-armv8,+fp16fml,+fptoint,+fullfp16,+jsconv,+lse,+neon,+pauth,+perfmon,+predres,+ras,+rcpc,+rdm,+sb,+sha2,+sha3,+specrestrict,+ssbs,+v8.1a,+v8.2a,+v8.3a,+v8.4a,+v8a" "tune-cpu"="apple-m5" }
attributes #3 = { nounwind }

!llvm.module.flags = !{!14, !15, !16, !17, !18, !19}
!llvm.dbg.cu = !{!2}
!llvm.ident = !{!20}
!llvm.errno.tbaa = !{!21}

!0 = !DIGlobalVariableExpression(var: !1, expr: !DIExpression())
!1 = distinct !DIGlobalVariable(name: "table", scope: !2, file: !5, line: 5, type: !6, isLocal: false, isDefinition: true)
!2 = distinct !DICompileUnit(language: DW_LANG_C11, file: !3, producer: "Homebrew clang version 23.1.2", isOptimized: true, runtimeVersion: 0, emissionKind: FullDebug, globals: !4, splitDebugInlining: false, nameTableKind: Apple, sysroot: "/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk", sdk: "MacOSX26.sdk")
!3 = !DIFile(filename: "/tmp/claude-501/facts-guard.c", directory: "/Users/duna/Developer/rllvm", checksumkind: CSK_MD5, checksum: "5c5f79c3795e9593d832aa618eee2cd6")
!4 = !{!0}
!5 = !DIFile(filename: "/tmp/claude-501/facts-guard.c", directory: "", checksumkind: CSK_MD5, checksum: "5c5f79c3795e9593d832aa618eee2cd6")
!6 = !DICompositeType(tag: DW_TAG_array_type, baseType: !7, size: 128, elements: !12)
!7 = !DIDerivedType(tag: DW_TAG_const_type, baseType: !8)
!8 = !DIDerivedType(tag: DW_TAG_pointer_type, baseType: !9, size: 64)
!9 = !DISubroutineType(types: !10)
!10 = !{!11, !11}
!11 = !DIBasicType(name: "int", size: 32, encoding: DW_ATE_signed)
!12 = !{!13}
!13 = !DISubrange(count: 2)
!14 = !{i32 2, !"SDK Version", [2 x i32] [i32 26, i32 5]}
!15 = !{i32 7, !"Dwarf Version", i32 5}
!16 = !{i32 2, !"Debug Info Version", i32 3}
!17 = !{i32 8, !"PIC Level", i32 2}
!18 = !{i32 7, !"uwtable", i32 1}
!19 = !{i32 7, !"frame-pointer", i32 4}
!20 = !{!"Homebrew clang version 23.1.2"}
!21 = !{!22, !23, i64 0}
!22 = !{!"__libc_errno", !23, i64 0}
!23 = !{!"int", !24, i64 0}
!24 = !{!"omnipotent char", !25, i64 0}
!25 = !{!"Simple C/C++ TBAA"}
!26 = distinct !DISubprogram(name: "one", scope: !5, file: !5, line: 3, type: !9, scopeLine: 3, flags: DIFlagPrototyped | DIFlagAllCallsDescribed, spFlags: DISPFlagLocalToUnit | DISPFlagDefinition | DISPFlagOptimized, unit: !2, retainedNodes: !27, keyInstructions: true)
!27 = !{!28}
!28 = !DILocalVariable(name: "x", arg: 1, scope: !26, file: !5, line: 3, type: !11)
!29 = !DILocation(line: 0, scope: !26)
!30 = !DILocation(line: 3, column: 25, scope: !26, atomGroup: 1, atomRank: 1)
!31 = distinct !DISubprogram(name: "two", scope: !5, file: !5, line: 4, type: !9, scopeLine: 4, flags: DIFlagPrototyped | DIFlagAllCallsDescribed, spFlags: DISPFlagLocalToUnit | DISPFlagDefinition | DISPFlagOptimized, unit: !2, retainedNodes: !32, keyInstructions: true)
!32 = !{!33}
!33 = !DILocalVariable(name: "x", arg: 1, scope: !31, file: !5, line: 4, type: !11)
!34 = !DILocation(line: 0, scope: !31)
!35 = !DILocation(line: 4, column: 34, scope: !31, atomGroup: 1, atomRank: 2)
!36 = !DILocation(line: 4, column: 25, scope: !31, atomGroup: 1, atomRank: 1)
!37 = distinct !DISubprogram(name: "dispatch", scope: !5, file: !5, line: 6, type: !38, scopeLine: 6, flags: DIFlagPrototyped | DIFlagAllCallsDescribed, spFlags: DISPFlagDefinition | DISPFlagOptimized, unit: !2, retainedNodes: !40, keyInstructions: true)
!38 = !DISubroutineType(types: !39)
!39 = !{!11, !11, !11}
!40 = !{!41, !42}
!41 = !DILocalVariable(name: "i", arg: 1, scope: !37, file: !5, line: 6, type: !11)
!42 = !DILocalVariable(name: "x", arg: 2, scope: !37, file: !5, line: 6, type: !11)
!43 = !DILocation(line: 0, scope: !37)
!44 = !DILocation(line: 6, column: 37, scope: !37)
!45 = !{!46, !46, i64 0}
!46 = !{!"any pointer", !24, i64 0}
!47 = !DILocation(line: 6, column: 37, scope: !37, atomGroup: 1, atomRank: 2)
!48 = !DILocation(line: 6, column: 30, scope: !37, atomGroup: 1, atomRank: 1)
!49 = distinct !DISubprogram(name: "main", scope: !5, file: !5, line: 7, type: !50, scopeLine: 7, flags: DIFlagPrototyped | DIFlagAllCallsDescribed, spFlags: DISPFlagDefinition | DISPFlagOptimized, unit: !2, keyInstructions: true)
!50 = !DISubroutineType(types: !51)
!51 = !{!11}
!52 = !DILocalVariable(name: "a", arg: 1, scope: !53, file: !5, line: 2, type: !11)
!53 = distinct !DISubprogram(name: "add", scope: !5, file: !5, line: 2, type: !38, scopeLine: 2, flags: DIFlagPrototyped | DIFlagAllCallsDescribed, spFlags: DISPFlagLocalToUnit | DISPFlagDefinition | DISPFlagOptimized, unit: !2, retainedNodes: !54, keyInstructions: true)
!54 = !{!52, !55}
!55 = !DILocalVariable(name: "b", arg: 2, scope: !53, file: !5, line: 2, type: !11)
!56 = !DILocation(line: 0, scope: !53, inlinedAt: !57)
!57 = distinct !DILocation(line: 7, column: 25, scope: !49)
!58 = !DILocation(line: 2, column: 77, scope: !53, inlinedAt: !57, atomGroup: 1, atomRank: 2)
!59 = !DILocation(line: 7, column: 35, scope: !49, atomGroup: 1, atomRank: 2)
!60 = !DILocation(line: 7, column: 18, scope: !49, atomGroup: 1, atomRank: 1)
!61 = !DISubprogram(name: "outer", scope: !5, file: !5, line: 1, type: !9, flags: DIFlagPrototyped, spFlags: DISPFlagOptimized)
