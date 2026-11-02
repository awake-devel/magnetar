#!/bin/bash
# Generates the video recon corpus used by tests/video_corpus.rs (ffprobe is the oracle there).
# Usage: gen_corpus2.sh OUTDIR. Needs ffmpeg (libx265, libx264, libvpx-vp9, libsvtav1) and python3.
# Nothing here is checked in: the files are 1-15 MB each and stay out of the repo.
#
# Premium audio: ffmpeg encodes TrueHD (5.1 at most), E-AC-3 and DTS core, but cannot produce
# Atmos or DTS-HD. thd_patch.py / eac3_patch.py rewrite the encoder output by hand, from the
# bitstream layout (TrueHD major sync: 4 substreams + 16-channel presentation bit; E-AC-3: addbsi
# with flag_ec3_extension_type_a). ffprobe then reports "Dolby TrueHD + Dolby Atmos" /
# "Dolby Digital Plus + Dolby Atmos" for them. These are spec-derived vectors, not real Atmos.
set -euo pipefail
OUT=${1:?usage: gen_corpus2.sh OUTDIR}
HERE="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$OUT"
cd "$OUT"
F="ffmpeg -hide_banner -loglevel error -y"
T=10

# Subtitles.
[ -s subs.srt ] || printf '1\n00:00:01,000 --> 00:00:03,000\nHello\n\n2\n00:00:05,000 --> 00:00:07,000\nWorld\n' > subs.srt

# Elementary audio streams.
[ -s a_thd51.thd ] || $F -f lavfi -i "sine=f=330:r=48000,aformat=channel_layouts=5.1" -t $T -c:a truehd -strict -2 -f truehd a_thd51.thd 2>/dev/null
[ -s a_thd_atmos.thd ] || python3 -I "$HERE/thd_patch.py" a_thd51.thd a_thd_atmos.thd 4 0xbc
[ -s a_eac3.eac3 ] || $F -f lavfi -i "sine=f=300:r=48000,aformat=channel_layouts=5.1" -t $T -c:a eac3 -b:a 384k a_eac3.eac3
[ -s a_eac3_joc.eac3 ] || python3 -I "$HERE/eac3_patch.py" a_eac3.eac3 a_eac3_joc.eac3 0110
[ -s a_eac3_plain.eac3 ] || python3 -I "$HERE/eac3_patch.py" a_eac3.eac3 a_eac3_plain.eac3 00
[ -s a_dts.dts ] || $F -f lavfi -i "sine=f=440:r=48000,aformat=channel_layouts=5.1" -t $T -c:a dca -strict -2 a_dts.dts
[ -s a_ac3.ac3 ] || $F -f lavfi -i "sine=f=500:r=48000,aformat=channel_layouts=5.1" -t $T -c:a ac3 -b:a 448k a_ac3.ac3

# Video: noisy content so the first keyframe is large (the first audio block sits behind it).
[ -s v_hevc_hdr10.mkv ] || $F -f lavfi -i "testsrc2=s=1280x720:r=24,noise=alls=30:allf=t,format=yuv420p10le" -t $T \
  -c:v libx265 -preset ultrafast -x265-params "log-level=error:hdr10=1:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400" \
  -b:v 6M v_hevc_hdr10.mkv
[ -s v_h264.mkv ] || $F -f lavfi -i "testsrc2=s=1280x720:r=25,noise=alls=20:allf=t" -t $T -c:v libx264 -preset ultrafast -b:v 4M -pix_fmt yuv420p v_h264.mkv

A="-i a_thd_atmos.thd -i a_eac3_joc.eac3 -i a_dts.dts -i a_ac3.ac3"
LANGS="-metadata:s:a:0 language=eng -metadata:s:a:1 language=fre -metadata:s:a:2 language=ger -metadata:s:a:3 language=spa"

# MKV: HEVC HDR10 + TrueHD Atmos + E-AC-3 JOC + DTS + AC-3 + AAC + 2 subtitles (one forced).
[ -s mkv_premium.mkv ] || $F -i v_hevc_hdr10.mkv $A -f lavfi -i "sine=r=48000" -i subs.srt -t $T \
  -map 0 -map 1 -map 2 -map 3 -map 4 -map 5 -map 6 -map 6 -c:v copy -c:a copy -c:a:4 aac -b:a:4 96k -c:s:0 srt -c:s:1 ass \
  $LANGS -metadata:s:a:4 language=jpn -metadata:s:s:0 language=eng -metadata:s:s:1 language=fre -disposition:s:1 forced -strict -2 mkv_premium.mkv
# MKV with the plain (no Atmos) E-AC-3 and the unpatched TrueHD.
[ -s mkv_plain_audio.mkv ] || $F -i v_h264.mkv -i a_thd51.thd -i a_eac3_plain.eac3 -t $T -map 0 -map 1 -map 2 -c copy -strict -2 mkv_plain_audio.mkv
# MP4: moov at the end, faststart and fragmented (with mfra).
[ -s mp4_premium_end.mp4 ] || $F -i v_hevc_hdr10.mkv -i a_eac3_joc.eac3 -i a_ac3.ac3 -i a_thd_atmos.thd -t $T -map 0 -map 1 -map 2 -map 3 -c copy -tag:v hvc1 \
  -metadata:s:a:0 language=eng -metadata:s:a:1 language=fre -metadata:s:a:2 language=ger -strict -2 mp4_premium_end.mp4
[ -s mp4_premium_fast.mp4 ] || $F -i mp4_premium_end.mp4 -map 0 -c copy -movflags +faststart -strict -2 mp4_premium_fast.mp4
[ -s mp4_frag.mp4 ] || $F -i mp4_premium_end.mp4 -map 0 -c copy -movflags frag_keyframe+delay_moov -strict -2 mp4_frag.mp4
[ -s mp4_dts.mp4 ] || $F -i v_h264.mkv -i a_dts.dts -i a_eac3_plain.eac3 -t $T -map 0 -map 1 -map 2 -c copy -strict -2 mp4_dts.mp4
# M2TS (Blu-ray style 192-byte packets) and plain TS.
[ -s m2ts_premium.m2ts ] || $F -i v_hevc_hdr10.mkv $A -t $T -map 0 -map 1 -map 2 -map 3 -map 4 -c copy $LANGS -mpegts_m2ts_mode 1 -strict -2 m2ts_premium.m2ts
[ -s ts_h264.ts ] || $F -i v_h264.mkv -i a_eac3_joc.eac3 -i a_ac3.ac3 -f lavfi -i "sine=r=48000" -t $T -map 0 -map 1 -map 2 -map 3 -c copy -c:a:2 aac \
  -metadata:s:a:0 language=eng -metadata:s:a:1 language=ita ts_h264.ts
# WebM VP9 profile 2 (10-bit) + Opus; VP9 profile 0 in MKV.
[ -s webm_vp9_p2.webm ] || $F -f lavfi -i "testsrc2=s=640x360:r=30,format=yuv420p10le" -f lavfi -i "sine=r=48000" -t 5 -c:v libvpx-vp9 -profile:v 2 -b:v 1M -deadline realtime -cpu-used 8 -c:a libopus webm_vp9_p2.webm
[ -s mkv_vp9_p0.mkv ] || $F -f lavfi -i "testsrc2=s=640x360:r=30" -f lavfi -i "sine=r=44100" -t 5 -c:v libvpx-vp9 -b:v 1M -deadline realtime -cpu-used 8 -c:a flac mkv_vp9_p0.mkv
# AV1 10-bit with mastering metadata (SVT-AV1 writes it in metadata OBUs).
[ -s mkv_av1_hdr.mkv ] || $F -f lavfi -i "testsrc2=s=640x360:r=24,format=yuv420p10le" -t 3 -c:v libsvtav1 -preset 12 \
  -svtav1-params "enable-hdr=1:color-primaries=9:transfer-characteristics=16:matrix-coefficients=9:mastering-display=G(0.265,0.690)B(0.150,0.060)R(0.680,0.320)WP(0.3127,0.3290)L(1000,0.0001):content-light=1000,400" mkv_av1_hdr.mkv
# AVI: MPEG-4 ASP (XviD tag) + MP3, and AC-3.
[ -s avi_xvid.avi ] || $F -f lavfi -i "testsrc2=s=640x352:r=25" -f lavfi -i "sine=r=44100" -t $T -c:v mpeg4 -vtag XVID -q:v 4 -c:a libmp3lame -b:a 128k avi_xvid.avi
[ -s avi_ac3.avi ] || $F -f lavfi -i "testsrc2=s=640x352:r=25" -i a_ac3.ac3 -t $T -c:v mpeg4 -vtag DX50 -bf 1 -c:a copy avi_ac3.avi
# MOV with Macintosh language codes.
[ -s mov_langs.mov ] || $F -f lavfi -i "testsrc2=s=640x360:r=30" -f lavfi -i "sine=r=48000" -t $T -map 0 -map 1 -map 1 -map 1 -c:v libx264 -preset ultrafast -profile:v baseline -c:a aac \
  -metadata:s:a:0 language=fre -metadata:s:a:1 language=jpn -metadata:s:a:2 language=dut mov_langs.mov
ls -la "$OUT"
