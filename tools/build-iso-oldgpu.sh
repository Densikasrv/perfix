#!/usr/bin/env bash
# Сборка загрузочного ISO Perfix (Модифицирован для старых видеокарт).
#
# Запуск (из любого каталога):
#     bash tools/build-iso.sh
#
# Требования: rustup с toolchain nightly и компонентом rust-src,
# nasm, GNU binutils, grub (grub-mkrescue), xorriso.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

KERNEL_DIR="$ROOT/kernel"
BUILD_DIR="$ROOT/build"
TARGET_SPEC="$KERNEL_DIR/i686-perfix.json"
TARGET_DIR="$KERNEL_DIR/target"
ELF="$TARGET_DIR/i686-perfix/debug/perfix"
ISO="$BUILD_DIR/perfix-oldgpu.iso"

for tool in nasm cargo rustc grub-mkrescue xorriso; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "error: не найден '$tool'" >&2
        exit 1
    fi
done

if ! rustc +nightly --version >/dev/null 2>&1; then
    echo "error: нет toolchain nightly (rustup toolchain install nightly)" >&2
    exit 1
fi

if ! rustup component list --toolchain nightly --installed 2>/dev/null | grep -q '^rust-src'; then
    echo "error: нет компонента rust-src (rustup component add rust-src --toolchain nightly)" >&2
    exit 1
fi

mkdir -p "$TARGET_DIR" "$BUILD_DIR/iso/boot/grub"

echo "[1/4] Объектные файлы загрузчика"
nasm -f elf32 "$ROOT/boot/boot.asm" -o "$TARGET_DIR/boot.o"
nasm -f elf32 "$ROOT/boot/gdt.asm" -o "$TARGET_DIR/boot-gdt.o"
nasm -f elf32 "$ROOT/boot/idt.asm" -o "$TARGET_DIR/boot-idt.o"

echo "[2/4] Сборка ядра"
export RUSTFLAGS="-C link-arg=-T$KERNEL_DIR/linker.ld \
-C link-arg=-m -C link-arg=elf_i386 \
-C link-arg=$TARGET_DIR/boot.o \
-C link-arg=$TARGET_DIR/boot-gdt.o \
-C link-arg=$TARGET_DIR/boot-idt.o"

# boot.o и другие ассемблерные объекты не отслеживаются cargo,
# поэтому принудительно обновляем метку времени исходника ядра.
touch "$KERNEL_DIR/src/main.rs"

# Набор флагов зависит от версии cargo: в одних nightly json-спека цели
# требует -Z json-target-spec, в других флаг уже удалён. Поэтому сборка
# сначала идёт без флага и повторяется с ним только при явной ошибке.
build_kernel() {
    cargo +nightly build \
        --manifest-path "$KERNEL_DIR/Cargo.toml" \
        "$@" \
        --target "$TARGET_SPEC"
}

set +e
BUILD_OUTPUT="$(build_kernel -Z build-std=core 2>&1)"
BUILD_STATUS=$?
set -e

printf '%s\n' "$BUILD_OUTPUT"

if [ "$BUILD_STATUS" -ne 0 ]; then
    if printf '%s' "$BUILD_OUTPUT" | grep -q 'json-target-spec'; then
        echo "[2/4] Повторная сборка с -Z json-target-spec"
        build_kernel -Z build-std=core -Z json-target-spec
    else
        exit "$BUILD_STATUS"
    fi
fi

if [ ! -f "$ELF" ]; then
    echo "error: не найден $ELF" >&2
    exit 1
fi

echo "[3/4] Подготовка каталога ISO"
rm -rf "$BUILD_DIR/iso"
mkdir -p "$BUILD_DIR/iso/boot/grub"

cp "$ELF" "$BUILD_DIR/iso/boot/perfix.elf"

# Пишем конфиг, который вообще не знает слов "video", "vbe" или "gfx"
cat > "$BUILD_DIR/iso/boot/grub/grub.cfg" <<'EOF'
# Меню будет висеть 3 секунды. Если ничего не нажать, включится графика.
set timeout=0
set default=0

menuentry "Perfix (Safe Text Mode (Old GPU))" {
    # Явно приказываем GRUB проигнорировать графический запрос из boot.asm
    # Это переведет ядро в текстовый режим без инициализации видеочипа VBE
    set gfxpayload=text
    multiboot /boot/perfix.elf
    boot
}
EOF



echo "[4/4] grub-mkrescue (Текстовая сборка)"
# Мы принудительно исключаем модули "vbe", "video", "all_video", "video_bochs", "video_cirrus".
# GRUB физически не сможет переключить видеокарту 1999 года из текстового режима BIOS.
grub-mkrescue \
    --modules="multiboot biosdisk iso9660 ext2 normal boot cat minicmd ls" \
    --fonts="" \
    --locales="" \
    --themes="" \
    -o "$ISO" \
    -d /usr/lib/grub/i386-pc \
    "$BUILD_DIR/iso"

echo
echo "Готово: $ISO"
