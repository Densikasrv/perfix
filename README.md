# Perfix — сборка ядра

Perfix — экспериментальное 32-битное ядро операционной системы для архитектуры x86, написанное на Rust без стандартной библиотеки (`no_std`). Загрузка выполняется через GRUB с использованием Multiboot 1.

Это руководство описывает сборку и запуск Perfix на Linux с использованием CachyOS, Rust nightly, GRUB и QEMU.

## 1. Требования

Для сборки потребуются:

- Linux x86-64
- Rust и Cargo через rustup
- Rust nightly и компонент `rust-src`
- NASM
- GNU binutils
- GRUB и `grub-mkrescue`
- xorriso
- QEMU с поддержкой i386

В Arch-based дистрибутиве установите необходимые системные инструменты через `pacman`. Названия пакетов QEMU и GRUB могут зависеть от состава репозиториев:

```bash
sudo pacman -S --needed rustup rust-src nasm binutils grub xorriso
```

Установите QEMU, если он ещё не установлен, выбрав пакет с поддержкой системной эмуляции i386.

Если системный пакет Rust конфликтует с rustup, не устанавливайте их вслепую одновременно: используйте одну согласованную установку Rust.

## 2. Настройка Rust

Установите nightly toolchain:

```bash
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
```

Проверьте установку:

```bash
rustc +nightly --version
cargo +nightly --version
```

Perfix использует собственную конфигурацию цели `kernel/i686-perfix.json`, поэтому для сборки применяется nightly Cargo с флагом `-Z json-target-spec`.

## 3. Получение исходников

Клонируйте репозиторий:

```bash
git clone https://github.com/Densikasrv/perfix.git
cd perfix
```

Посмотрите структуру проекта:

boot/ — загрузочный код на ассемблере.

kernel/src/ — исходный код ядра на Rust.

kernel/linker.ld — сценарий компоновщика.

kernel/i686-perfix.json — конфигурация цели компиляции.

build/ — каталог для сборочных артефактов.

4. Сборка ядра

Перед сборкой должны быть доступны объектные файлы загрузчика, используемые линковщиком:

kernel/target/boot.o

kernel/target/boot-gdt.o

kernel/target/boot-idt.o

Они должны быть созданы предусмотренным проектом способом из соответствующих файлов в boot/.

Из корня репозитория выполните:

set -lx RUSTFLAGS "-C link-arg=-T/home/dzianis/perfix/kernel/linker.ld -C link-arg=-m -C link-arg=elf_i386 -C link-arg=/home/dzianis/perfix/kernel/target/boot.o -C link-arg=/home/dzianis/perfix/kernel/target/boot-gdt.o -C link-arg=/home/dzianis/perfix/kernel/target/boot-idt.o"

cargo +nightly build \
    --manifest-path kernel/Cargo.toml \
    -Z build-std=core \
    -Z json-target-spec \
    --target kernel/i686-perfix.json

Важно: путь /home/dzianis/perfix относится к исходной среде разработки автора. Если проект находится в другом каталоге, замените абсолютные пути в RUSTFLAGS на соответствующие пути вашей системы.

При успешной сборке ELF-файл ядра появится по пути:

kernel/target/i686-perfix/debug/perfix

5. Создание загрузочного ISO

Подготовьте каталог ISO:

mkdir -p build/iso/boot/grub

Скопируйте скомпилированное ядро:

cp kernel/target/i686-perfix/debug/perfix build/iso/boot/perfix.elf

В каталоге build/iso/boot/grub/ должен находиться grub.cfg, содержащий конфигурацию загрузки ядра через Multiboot 1. Если файл отсутствует, создайте его со следующим содержимым:

set timeout=0
set default=0

menuentry "Perfix" {
    multiboot /boot/perfix.elf
    boot
}

Создайте ISO:

grub-mkrescue \
    --modules="multiboot" \
    --fonts="" \
    --locales="" \
    --themes="" \
    -o build/perfix-min.iso \
    -d /usr/lib/grub/i386-pc \
    build/iso

Готовый образ будет находиться в build/perfix-min.iso.

6. Запуск в QEMU

Запустить ядро без дополнительного тестового диска можно так:

qemu-system-i386 \
    -cdrom build/perfix-min.iso \
    -m 128M

Если прошивка не начинает загрузку с ISO автоматически, укажите порядок загрузки:

qemu-system-i386 \
    -cdrom build/perfix-min.iso \
    -boot order=d \
    -m 128M

Для проверки ATA и экспериментальной работы с exFAT подключите тестовый образ диска:

qemu-system-i386 \
    -drive file=build/perfix-disk.img,format=raw,if=ide,index=0 \
    -cdrom build/perfix-min.iso \
    -boot order=d \
    -m 128M

Образ build/perfix-disk.img используется для тестирования дисковых операций. Он не требуется для обычной загрузки ядра.

7. Возможности ядра

В текущей экспериментальной версии реализованы:

Загрузка через GRUB и Multiboot 1.

Вывод текста в VGA text mode.

Обработка ввода с PS/2-клавиатуры.

Командная оболочка.

GDT и IDT.

Обработчики исключений процессора.

Базовое включение paging с identity mapping первых 4 МиБ.

Чтение секторов ATA PIO.

Чтение основной информации из загрузочного сектора exFAT.

Экспериментальное чтение записей корневого каталога exFAT.

Реализация файловой системы находится в разработке. Поддержка полноценного чтения файлов, длинных цепочек кластеров и записи данных пока не должна считаться завершённой.

8. Ограничения

Perfix — учебный проект, а не готовая операционная система общего назначения.

Основной проверенной средой запуска является QEMU. Совместимость с реальным оборудованием, включая старые процессоры, необходимо проверять отдельно.

Перед экспериментами с реальными накопителями используйте тестовые образы и резервные копии.

Лицензия

Условия использования и распространения определяются лицензией, указанной в репозитории, если она добавлена.
