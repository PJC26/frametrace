# FFmpeg (BtbN autobuild-2026-09-30-13-08) — statically linked components

Informational only. FrameTrace does not distribute this binary. This table lists what the BtbN FFmpeg-Builds tag autobuild-2026-09-30-13-08 (win64, variant lgpl, addin 8.1) statically links, derived from its scripts.d selection rules — not read out of the binary. It exists so an examiner who installs that build with `frametrace tools install ffmpeg` can see which licences the file carries. The Windows CI install-path smoke records the real `-buildconf` for comparison. Kept for the option-A (self-built LGPL FFmpeg) plan, docs/PLAN_TOOLS_C_2026-10-20.md appendix A.

## Statically linked libraries

Derived from the `scripts.d` of BtbN tag `autobuild-2026-09-30-13-08` with its
`generate.sh` selection rules for target `win64`, variant `lgpl`, addin `8.1`
(each script's `ffbuild_enabled`; a directory stage is built only when its last
script is enabled). The commit is each script's `SCRIPT_COMMIT` (or tag /
`SCRIPT_REV`); the license is what the library's root license files say at that
commit.
"version per file headers" means the root file is the bare license text and the
exact version / "or later" terms are in the source headers. Disabled for this
variant (not in the binary): x264, x265, xvid, xavs2, davs2, fdk-aac, avisynth,
frei0r, rubberband, vidstab, libdvdcss/libdvdread/libdvdnav, libcurl (and its
libpsl / nghttp2 / nghttp3 / ngtcp2 / zstd), whisper, pulseaudio, X11.

| Component | Upstream | Pinned commit / tag | License (from the fetched files) |
| --- | --- | --- | --- |
| AMF | https://github.com/GPUOpen-LibrariesAndSDKs/AMF.git | `6277e353fd625121a8f627b1d0540323ef372a49` | MIT |
| aom | https://aomedia.googlesource.com/aom | `b4ce2cd097b00921b0327bfb5f07b5f8361ade99` | BSD-2-Clause |
| brotli | https://github.com/google/brotli.git | `a3abcbee0d945e51dddeec81e647ffe0cde182c2` | MIT |
| chromaprint | https://github.com/acoustid/chromaprint.git | `aed8eba2202dd9d7b3b0a56c77904cc805490d72` | MIT (own code); LICENSE.md states the combined work is LGPL-2.1 because it includes FFmpeg code |
| dav1d | https://code.videolan.org/videolan/dav1d.git | `9711965b60bb692ae24004659acf61f5c7d9ed61` | BSD-2-Clause |
| nv-codec-headers (ffnvcodec) | https://github.com/FFmpeg/nv-codec-headers.git | `eddcea9e27f6b772057c9b3f87de2cc1737faffc` | MIT (per licence comment in source headers; no licence file at repo root) |
| fftw3 | https://github.com/FFTW/fftw3.git | `93ed4c786934aec9946f8dda4b4e3eb08f8be41c` | GPL-2.0-or-later |
| freetype (fonts) | https://gitlab.freedesktop.org/freetype/freetype.git | `d333439633039de426f943f28a2926c7f97b5ae5` | FTL OR GPL-2.0-or-later |
| fontconfig (fonts) | https://gitlab.freedesktop.org/fontconfig/fontconfig.git | `bd8f7b597de96761750d0365abb49b19d2f8d5c3` | HPND-sell-variant (permission text in COPYING; COPYING also lists other MIT-style notices) |
| harfbuzz (fonts) | https://github.com/harfbuzz/harfbuzz.git | `b3bab62307017934dd7f532c52a696fefb12b3ad` | MIT ('Old MIT' per COPYING) |
| fribidi | https://github.com/fribidi/fribidi.git | `4c914a92e94a9fe4f30ae83a1130099841566448` | LGPL-2.1 (COPYING is LGPL-2.1 text; version per file headers) |
| game-music-emu (gme) | https://github.com/libgme/game-music-emu.git | `f68963b1de0633149b05732113d3c3113c6f63a1` | LGPL-2.1 (license.txt; version per file headers) |
| gmplib (gmp) | https://github.com/BtbN/gmplib.git | `763b40df71c0f4d5f5d7b33dc55038c8c9377ecf` | LGPL-3.0 OR GPL-2.0 (dual; COPYING.LESSERv3 + COPYINGv2 + COPYINGv3; 'or later' per file headers) |
| kvazaar | https://github.com/ultravideo/kvazaar.git | `2b06691bb5844404c0e703f12a5d7b0fee914ec7` | BSD-3-Clause |
| LCEVCdec | https://github.com/v-novaltd/LCEVCdec.git | `17804ac54db8fbb42717f3275b1e73f3c0b067d3` | BSD-3-Clause-Clear |
| aribb24 (libaribb24) | https://github.com/nkoriyama/aribb24.git | `5e9be272f96e00f15a2f3c5f8ba7e124862aec38` | LGPL-3.0 (COPYING is LGPL-3.0 text; version per file headers) |
| libaribcaption | https://github.com/xqq/libaribcaption.git | `c64c23b8905ba514b87c9789269e9f66f949ffe0` | MIT |
| libass | https://github.com/libass/libass.git | `f61db567e6593df3470e91594bcd4ad2d0473aff` | ISC |
| libbluray | https://code.videolan.org/videolan/libbluray.git | `a24f4fad4d62893de647abc8671397747b2359dd` | LGPL-2.1 (version per file headers) |
| libffi | https://github.com/libffi/libffi.git | `bc553867367246d140cd156f060bd0409f57f157` | MIT |
| libiconv | https://git.savannah.gnu.org/git/libiconv.git | `1df3087ba8110c7f3ed3eb5f8869b814dbbe00b0` | LGPL-2.1 (COPYING.LIB; version per file headers) |
| Little-CMS (libjxl) | https://github.com/mm2/Little-CMS.git | `a0b0d7a69b13b461bdbd9cff9f7f1d59ccd1858c` | MIT |
| libjxl | https://github.com/libjxl/libjxl.git | `b87738951c1254cd8cccaa6d47712ba735da56d8` | BSD-3-Clause |
| lame (libmp3lame) | https://svn.code.sf.net/p/lame/svn/trunk/lame | `svn rev 6835` | LGPL-2.0 (COPYING is GNU Library GPL v2.0 text; version per file headers) |
| ogg (libogg) | https://github.com/xiph/ogg.git | `06a5e0262cdc28aa4ae6797627a783b5010440f0` | BSD-3-Clause |
| opus (libopus) | https://github.com/xiph/opus.git | `503d81b138d76621aae4b12786e90de48aa8db3a` | BSD-3-Clause |
| libplacebo | https://code.videolan.org/videolan/libplacebo.git | `e2972fdd09adacd383656738d7d280f0cd84a761` | LGPL-2.1 (version per file headers) |
| libpng | https://github.com/glennrp/libpng.git | `964b4135949703b705fc760fc3fb546b86e5ab47` | libpng-2.0 (PNG Reference Library License v2); LICENSES/ contain MIT and MIT-0 for some files |
| mbedtls (librist) | https://github.com/ARMmbed/mbedtls.git | `v4.2.0` | Apache-2.0 OR GPL-2.0-or-later |
| librist | https://code.videolan.org/rist/librist.git | `4f45ef8f78983892d52ccd52d9f675435b23738f` | BSD-2-Clause |
| glib (librsvg) | https://github.com/GNOME/glib.git | `5a124d1e66a6c2dc56989303b4d6d4781c2952ed` | LGPL-2.1-or-later |
| cairo (librsvg) | https://gitlab.freedesktop.org/cairo/cairo.git | `74755964edef651691d5cd36ff140108298e4e6e` | LGPL-2.1 OR MPL-1.1 |
| pango (librsvg) | https://github.com/GNOME/pango.git | `8e74c27c113c98664b77272d16b14bbce542c184` | LGPL-2.0 (COPYING is GNU Library GPL v2 text; version per file headers) |
| librsvg | https://github.com/GNOME/librsvg.git | `7612431eb02dc009319094f8513d63c1faaecfb4` | LGPL-2.1 (COPYING.LIB is LGPL-2.1 text; version per file headers) |
| libsamplerate | https://github.com/libsndfile/libsamplerate.git | `0844c208f683527c08ea8a80acc13b398aa9c8bf` | BSD-2-Clause |
| libssh-mirror (libssh) | https://gitlab.com/libssh/libssh-mirror.git | `76e0a8639e61436c28d877586f65a7707edc6ee5` | LGPL-2.1 (version per file headers) |
| theora (libtheora) | https://github.com/xiph/theora.git | `28fd5ec77f0ad0e07a371cef1047828116f6bd8a` | BSD-3-Clause |
| libudfread | https://code.videolan.org/videolan/libudfread.git | `b0bc6957e7e07d5f35391f210596d9eb71cffdd9` | LGPL-2.1 (version per file headers) |
| libunibreak | https://github.com/adah1972/libunibreak.git | `28a2756b864c343f438cd22537d49d394d4666a5` | Zlib |
| vorbis (libvorbis) | https://github.com/xiph/vorbis.git | `1b75110b5a2754ba1931d82dd83cb822b266a21d` | BSD-3-Clause |
| libvpx | https://chromium.googlesource.com/webm/libvpx | `5e680f30801d03c21078f8c4b772464752516211` | BSD-3-Clause |
| libwebp | https://chromium.googlesource.com/webm/libwebp | `5c168cd23b1872969c38e34b01b04dcf29849288` | BSD-3-Clause |
| libxml2 | https://github.com/GNOME/libxml2.git | `91586dc6742ab335682235120363f6e126eea5e2` | MIT |
| libzmq | https://github.com/zeromq/libzmq.git | `46493370217ac135246617fa2f6ac819d8b61bfc` | MPL-2.0 |
| lv2 (lilv) | https://github.com/lv2/lv2.git | `3c57dae600a5ad8d05acd53ee3490f7d91cb7be6` | ISC (COPYING); LICENSES/ also contain 0BSD, Apache-2.0, CC-BY-1.0, CC-BY-4.0, MIT, W3C-19980720, W3C-20150513 for specific files |
| serd (lilv) | https://github.com/drobilla/serd.git | `b6053bb2a4533e6a8e3821c1061f140d5e666938` | ISC (COPYING); LICENSES/ also contain 0BSD, BSD-3-Clause |
| zix (lilv) | https://github.com/drobilla/zix.git | `17d632393f5addb6fa24dda2927b96515407cca7` | ISC (COPYING); LICENSES/ also contain 0BSD |
| sord (lilv) | https://github.com/drobilla/sord.git | `4b5232bb91e7f2f1b84df384494d073fabbcd312` | ISC (COPYING); LICENSES/ also contain 0BSD, BSD-3-Clause |
| sratom (lilv) | https://github.com/lv2/sratom.git | `9e8ee84eed502e6030ce8bb49694fd28542b08fc` | ISC (COPYING); LICENSES/ also contain 0BSD |
| lilv | https://github.com/lv2/lilv.git | `4b8f30055fa5cb4134a2eb45fc1555bc04289314` | ISC (COPYING); LICENSES/ also contain 0BSD |
| mingw-w64 (mingw) | https://git.code.sf.net/p/mingw-w64/mingw-w64.git | `57b595039040eaa15bece85b7cc71d952281b269` | ZPL-2.1 (overall) plus BSD/public-domain/LGPL parts and MIT-style winpthreads; see COPYING.MinGW-w64-runtime.txt for the binary-distribution notices |
| mingw-std-threads | https://github.com/meganz/mingw-std-threads.git | `c931bac289dd431f1dd30fc4a5d1a7be36668073` | BSD-2-Clause |
| libvpl (onevpl) | https://github.com/intel/libvpl.git | `674d015bcb294bc39fa276e99a652ea045423e82` | MIT |
| openal-soft (openal) | https://github.com/kcat/openal-soft.git | `c89b8cf7bba4822f230c69f0a6696a52e5322dca` | LGPL-2.0 (COPYING is GNU Library GPL v2 text; version per file headers); portions Apache-2.0, BSD-3-Clause, pffft licence |
| openapv | https://github.com/AcademySoftwareFoundation/openapv.git | `a58ce739be0dfb083643d929aee8f0e0ba9bdf63` | BSD-3-Clause |
| OpenCL-Headers (opencl) | https://github.com/KhronosGroup/OpenCL-Headers.git | `e6060189f4ebe8b52d885c37af71b9a50c272154` | Apache-2.0 |
| code (opencore-amr) | https://git.code.sf.net/p/opencore-amr/code | `7dba8c32238418ce0b316a852b2224df586ca896` | Apache-2.0 |
| openh264 | https://github.com/cisco/openh264.git | `8b2d28faade10d74d99ac80e199aef664c9c5a3b` | BSD-2-Clause |
| openjpeg | https://github.com/uclouvain/openjpeg.git | `8314119b067c0fc77834731168daaebd379fdb12` | BSD-2-Clause |
| openmpt | https://github.com/OpenMPT/openmpt.git | `f83cedb0cd5446e4dfaa83ac97e3087107e26767` | BSD-3-Clause |
| openssl | https://github.com/openssl/openssl.git | `openssl-3.6.4` | Apache-2.0 |
| pcre2 | https://github.com/PCRE2Project/pcre2.git | `09eb19dc1102b34e7557408f33318364cb97d2b0` | BSD-3-Clause WITH PCRE2-exception |
| pixman | https://gitlab.freedesktop.org/pixman/pixman.git | `96c04d1b87934dc4b9396197a2dff737698ab310` | MIT |
| rav1e | https://github.com/xiph/rav1e.git | `31435de9d76fddd38f6dcc31d4014574cebb2092` | BSD-2-Clause |
| SDL | https://github.com/libsdl-org/SDL.git | `b90ac95029d801c5abc59472ba8e2200dff31e1e` | Zlib |
| snappy | https://github.com/google/snappy.git | `9c28114a38866f6deeaa826db918293bc28ae410` | BSD-3-Clause |
| code (soxr) | https://git.code.sf.net/p/soxr/code | `945b592b70470e29f917f4de89b4281fbbd540c0` | LGPL-2.1-or-later |
| srt | https://github.com/Haivision/srt.git | `ff8ab25c57aece5b7351defe36dacc94fc28527f` | MPL-2.0 |
| SVT-AV1 (svtav1) | https://gitlab.com/AOMediaCodec/SVT-AV1.git | `5ed9ffbcb5086da23f30fe9bc5b00145358462da` | BSD-3-Clause-Clear |
| twolame | https://github.com/njh/twolame.git | `6fced852d4d5cfad58cf9dbe3ea619b08e87d398` | LGPL-2.1 (version per file headers) |
| uavs3d | https://github.com/uavs3/uavs3d.git | `0e20d2c291853f196c68922a264bcd8471d75b68` | BSD-3-Clause |
| libva (vaapi) | https://github.com/intel/libva.git | `6b07f7100512817f736967e899b8c26313c20623` | MIT |
| vmaf | https://github.com/Netflix/vmaf.git | `86da14d0306a138fd3f01319860b905169746516` | BSD-2-Clause-Patent |
| Vulkan-Headers (vulkan) | https://github.com/KhronosGroup/Vulkan-Headers.git | `v1.4.363` | Apache-2.0 OR MIT |
| Vulkan-Shim-Loader (vulkan) | https://github.com/BtbN/Vulkan-Shim-Loader.git | `65b3936528cd92eb4ea3de485d03f858a3850484` | MIT |
| shaderc (vulkan) | https://github.com/google/shaderc.git | `a8abeb0b8a9d4b11e3d59ca9f4550b8213e733ab` | Apache-2.0 |
| SPIRV-Cross (vulkan) | https://github.com/KhronosGroup/SPIRV-Cross.git | `aa217aeb6c9f0ace7a0ab233b28807edf45eb165` | Apache-2.0 (LICENSE); LICENSES/ also contain MIT, CC-BY-4.0, LicenseRef-KhronosFreeUse |
| SPIRV-Headers (vulkan) | https://github.com/KhronosGroup/SPIRV-Headers.git | `2b7475f6d664efbad4d4315c94ecec9640831e5b` | MIT (LICENSE lists exceptions; LICENSES/ contain MIT, CC-BY-4.0) |
| vvenc | https://github.com/fraunhoferhhi/vvenc.git | `b1cfb2ad495af6ae4cc214b7a060e8b4166c4629` | BSD-3-Clause-Clear |
| xz | https://github.com/tukaani-project/xz.git | `3b1efb04d17c3a9ef7f473d73af13f1531428ffe` | 0BSD (liblzma); COPYING lists GPL/LGPL files that are tools/build-system only |
| zimg | https://github.com/sekrit-twc/zimg.git | `67e0603271c080e22c8429856dd4a8a56587e61e` | WTFPL-2.0 |
| zlib | https://github.com/madler/zlib.git | `767c4c947852e143f582c85f14cf573411df1b35` | Zlib |
| zvbi | https://github.com/zapping-vbi/zvbi | `d3a5ee9f2b047bf16cd1ee5ccf6ec05ee75409d0` | GPL-2.0-or-later (default for files not otherwise specified) / src/* is LGPL-2.0+, except packet-830.* and pdc.* which are GPL-2.0-only (headers checked) / MIT, BSD-2-Clause for some files |
| gcc | https://github.com/gcc-mirror/gcc | `releases/gcc-16.2.0` | GPL-3.0-or-later WITH GCC-exception-3.1 |

Toolchain runtime parts linked into the executables: mingw-w64 CRT and
winpthreads (row `mingw-w64` above, notices in `COPYING.MinGW-w64-runtime.txt`
and `winpthreads/COPYING`) and the GCC 16.2.0 runtime (libgcc / libstdc++,
GPL-3.0-or-later WITH GCC Runtime Library Exception 3.1 — no obligations on the
resulting program; texts included). Build tools (binutils, the GCC compiler,
NASM, meson, …) are not part of the binary.
