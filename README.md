# HotkeyImageSorter

> **Minimalist app for fast image preview/edit/move to target directories with user defined hotkeys**

---

## Table of Contents

1. [Overview](#overview)
2. [Features](#features)
3. [Architecture](#architecture)
4. [Installation](#installation)
5. [Configuration](#configuration)
6. [Usage Guide](#usage-guide)
7. [Keyboard Shortcuts](#keyboard-shortcuts)
8. [Technical Details](#technical-details)
9. [Development](#development)
10. [Troubleshooting](#troubleshooting)

---

## Overview

**HotkeyImageSorter** is a desktop application built with [Tauri](https://tauri.app/) that enables rapid image organization through keyboard-driven workflows. Designed for photographers, designers, and anyone who needs to sort large batches of images efficiently, the application combines fast image preview with one-key file organization.

### Key Value Proposition

- ⚡ **Speed**: Navigate and sort images without touching the mouse
- 🎯 **Precision**: Custom hotkeys for each target directory
- 🔄 **Safety**: Undo/redo functionality for move operations
- 🖼️ **Preview**: Built-in image viewer with zoom, rotate, and crop tools

---

## Features

### Core Functionality

| Feature | Description |
|---------|-------------|
| **Hotkey Sorting** | Assign directories to keys A-Z for instant file organization |
| **Image Preview** | High-quality image rendering with metadata display |
| **Batch Navigation** | Navigate through entire folders with keyboard shortcuts |
| **Undo/Redo** | Reverse accidental moves with full history tracking |
| **Image Editing** | Rotate (90°), crop, and zoom capabilities |
| **Smart Sorting** | Sort by name, date, or size with ascending/descending order |
| **Persistent Config** | Settings saved between sessions (folders, hotkeys, preferences) |

### Supported Image Formats

The application supports all standard image formats handled by the system:
- JPEG/JPG
- PNG
- GIF
- BMP
- WebP
- TIFF

---

## Architecture

### Technology Stack

```
┌─────────────────────────────────────────────────────────────┐
│                      Frontend (JavaScript)                   │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │   app.js    │  │  index.html │  │   style.css         │  │
│  │  (UI Logic) │  │  (Structure)│  │   (Styling)         │  │
│  └─────────────┘  └─────────────┘  └─────────────────────┘  │
│                         Tauri API Bridge                      │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                       Backend (Rust)                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │ file_ops.rs │  │image_proc.rs│  │   config.rs         │  │
│  │(File Moves) │  │ (Edit Ops)  │  │  (Settings)         │  │
│  └─────────────┘  └─────────────┘  └─────────────────────┘  │
│                      Tauri Core Runtime                      │
└─────────────────────────────────────────────────────────────┘
```

### Component Breakdown

#### Frontend (`src/`)
| File | Purpose |
|------|---------|
| `app.js` | Main application logic, event handlers, state management |
| `index.html` | Application structure and UI elements |
| `style.css` / `styles.css` | Visual styling and responsive design |
| `main.js` | Application entry point and initialization |

#### Backend (`src-tauri/src/`)
| File | Purpose |
|------|---------|
| `main.rs` | Tauri application entry point |
| `lib.rs` | Core library exports and command registration |
| `file_ops.rs` | File system operations (move, list, delete) |
| `image_proc.rs` | Image processing (rotate, crop, metadata) |
| `config.rs` | Configuration management and persistence |
| `models.rs` | Data structures and type definitions |

### Data Flow

```
User Action → Frontend Event → Tauri Invoke → Rust Command → File System
     ↑                                                              │
     └────────────────────── Response ─────────────────────────────┘
```

---

## Installation

### Prerequisites

Before installing, ensure you have the following:

| Requirement | Version | Installation |
|-------------|---------|--------------|
| Node.js | 18+ | [nodejs.org](https://nodejs.org/) |
| Rust | 1.70+ | [rustup.rs](https://rustup.rs/) |
| Git | Latest | [git-scm.com](https://git-scm.com/) |

### Platform-Specific Requirements

#### Windows
```powershell
# Install WebView2 (usually pre-installed on Windows 10+)
# Install Visual Studio Build Tools with C++ workload
```

#### macOS
```bash
# Install Xcode Command Line Tools
xcode-select --install
```

#### Linux
```bash
# Ubuntu/Debian
sudo apt update
sudo apt install libwebkit2gtk-4.0-dev build-essential curl wget \
    libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev

# Fedora
sudo dnf install webkit2gtk3-devel openssl-devel curl wget \
    gtk3-devel libappindicator-gtk3-devel librsvg2-devel
```

### Installation Steps

```bash
# 1. Clone the repository
git clone https://github.com/Sucotasch/HotkeyImageSorter.git
cd HotkeyImageSorter

# 2. Install Node.js dependencies
npm install

# 3. Build and run in development mode
npm run dev

# 4. Build for production
npm run build
```

### Pre-built Binaries

Download pre-compiled binaries from the [Releases](https://github.com/Sucotasch/HotkeyImageSorter/releases) page:
- **Windows**: `.exe` installer or portable `.zip`
- **macOS**: `.dmg` or `.app` bundle
- **Linux**: `.deb`, `.rpm`, or `.AppImage`

---

## Configuration

### Initial Setup

1. **Launch the application**
2. **Select Source Folder**: Click "Select Folder" or press `Ctrl+O`
3. **Configure Hotkeys**: Open Settings (`Ctrl+,`) to assign directories to keys

### Configuration File

Settings are stored in a JSON configuration file (location varies by OS):

| OS | Configuration Path |
|----|-------------------|
| Windows | `%APPDATA%\HotkeyImageSorter\config.json` |
| macOS | `~/Library/Application Support/HotkeyImageSorter/config.json` |
| Linux | `~/.config/HotkeyImageSorter/config.json` |

### Configuration Schema

```json
{
  "last_folder": "/path/to/source/images",
  "last_file": "/path/to/last/viewed/image.jpg",
  "sort_by": "name",
  "sort_dir": 1,
  "hotkeys": {
    "a": "/path/to/category/a",
    "b": "/path/to/category/b",
    "c": "/path/to/category/c"
  }
}
```

### Hotkey Configuration

| Key | Default Action | Configurable |
|-----|---------------|--------------|
| A-Z | User-defined directory | ✅ Yes |
| 0-9 | Reserved | ❌ No |
| Special | System shortcuts | ❌ No |

---

## Usage Guide

### Quick Start Workflow

```
1. Open Application
       ↓
2. Select Source Folder (Ctrl+O)
       ↓
3. Navigate Images (← → or Arrow Keys)
       ↓
4. Press Hotkey (A-Z) to Sort
       ↓
5. Continue or Undo (Ctrl+Z) if Needed
```

### Main Functions

#### 1. Folder Selection

**Method 1**: Click the "Select Folder" button
**Method 2**: Press `Ctrl+O`

```javascript
// Behind the scenes
async function selectFolder() {
  const selected = await open({
    directory: true,
    multiple: false,
  });
  // Loads all images from selected directory
}
```

#### 2. Image Navigation

| Action | Shortcut |
|--------|----------|
| Next Image | `→` or `L` |
| Previous Image | `←` or `H` |
| First Image | `Home` |
| Last Image | `End` |

#### 3. Sorting Images

1. Configure target directories in Settings
2. View an image you want to sort
3. Press the assigned hotkey (A-Z)
4. Image moves to target directory automatically
5. Next image loads automatically

#### 4. Image Editing

##### Rotate
```
Ctrl+R or Ctrl+Shift+R
```
Rotates image 90° clockwise. Changes are saved to the original file.

##### Crop
```
1. Press Ctrl+C to activate crop mode
2. Drag crop region to desired area
3. Double-click to execute crop
4. Press Ctrl+C again to cancel
```

##### Zoom
```
Mouse Wheel: Zoom in/out
Ctrl+0: Reset zoom to 100%
```

#### 5. Undo/Redo Operations

| Action | Shortcut | Description |
|--------|----------|-------------|
| Undo | `Ctrl+Z` | Reverse last move operation |
| Redo | `Ctrl+X` | Reapply undone move |

```javascript
// Undo implementation
async function doUndo() {
  const undonePath = await invoke('undo_last_move');
  if (undonePath) {
    await loadFiles();
    refreshCurrentImage();
  }
}
```

#### 6. Delete Image

```
Delete or Backspace
```
Moves image to system trash (configurable in settings).

---

## Keyboard Shortcuts

### Navigation

| Shortcut | Action |
|----------|--------|
| `←` / `H` | Previous image |
| `→` / `L` | Next image |
| `Home` | First image |
| `End` | Last image |
| `Ctrl+O` | Open folder |

### Sorting

| Shortcut | Action |
|----------|--------|
| `A` - `Z` | Move to assigned directory |
| `Delete` | Delete current image |

### Editing

| Shortcut | Action |
|----------|--------|
| `Ctrl+R` | Rotate 90° clockwise |
| `Ctrl+C` | Toggle crop mode |
| `Mouse Wheel` | Zoom in/out |
| `Ctrl+0` | Reset zoom |

### System

| Shortcut | Action |
|----------|--------|
| `Ctrl+Z` | Undo last move |
| `Ctrl+X` | Redo move |
| `Ctrl+,` | Open settings |
| `Esc` | Close modal/dialog |

---

## Technical Details

### State Management

The application maintains state in a global object:

```javascript
const state = {
  folder: null,           // Source directory path
  images: [],             // Array of image metadata
  currentIndex: 0,        // Current image index
  currentPath: '',        // Current image full path
  config: {},             // User configuration
  sort: { by: 'name', dir: 1 },
  isCropActive: false,
  history: { undo: [], redo: [] }
};
```

### Image Loading Pipeline

```
File Path → Tauri convertFileSrc → Blob URL → Image Element → Render
                ↓
         Cache Buster (?t=timestamp)
                ↓
           Force Refresh
```

### File Operations

All file operations are executed through Tauri commands:

| Command | Parameters | Returns |
|---------|------------|---------|
| `list_files` | `{ dir: string }` | `ImageInfo[]` |
| `move_image` | `{ source, targetDir }` | `newPath` |
| `rotate_image` | `{ path, degrees }` | `void` |
| `undo_last_move` | `{}` | `undonePath` |
| `redo_last_move` | `{}` | `redonePath` |
| `save_config` | `{ config }` | `void` |
| `get_history_stats` | `{}` | `[undoCount, redoCount]` |

### Performance Considerations

- **Lazy Loading**: Images load on-demand, not all at once
- **Metadata Caching**: File info cached after initial scan
- **Efficient Rendering**: Only visible elements updated
- **Background Operations**: File moves don't block UI

---

## Development

### Project Structure

```
HotkeyImageSorter/
├── src/                    # Frontend source
│   ├── app.js             # Main application logic
│   ├── main.js            # Entry point
│   ├── index.html         # HTML structure
│   └── style.css          # Styles
├── src-tauri/             # Rust backend
│   ├── src/
│   │   ├── main.rs        # Application entry
│   │   ├── lib.rs         # Command exports
│   │   ├── file_ops.rs    # File operations
│   │   ├── image_proc.rs  # Image processing
│   │   ├── config.rs      # Configuration
│   │   └── models.rs      # Data models
│   ├── Cargo.toml         # Rust dependencies
│   └── tauri.conf.json    # Tauri configuration
├── package.json           # Node.js dependencies
└── README.md              # This file
```

### Development Commands

```bash
# Start development server
npm run dev

# Build for production
npm run build

# Build for specific platform
npm run tauri build -- --target x86_64-pc-windows-msvc
```

### Adding New Features

1. **Frontend**: Add UI elements in `index.html`, logic in `app.js`
2. **Backend**: Create Rust function in appropriate module
3. **Bridge**: Register command in `lib.rs`
4. **Invoke**: Call from frontend using `invoke('command_name', params)`

### Testing

```bash
# Run Rust tests
cd src-tauri && cargo test

# Run frontend tests (if configured)
npm test
```

---

## Troubleshooting

### Common Issues

#### Images Not Loading
```
Solution: Check file permissions and path accessibility
```

#### Hotkeys Not Working
```
Solution: 
1. Verify hotkey is configured in Settings
2. Ensure target directory exists
3. Check write permissions for target directory
```

#### Application Crashes on Startup
```
Solution:
1. Delete configuration file
2. Reinstall application
3. Check system requirements
```

#### Undo/Redo Not Working
```
Solution: History is session-based. Restarting clears history.
```

### Debug Mode

Enable debug logging:

```javascript
// In app.js
console.log('Debug:', state);
```

```rust
// In Rust backend
#[cfg(debug_assertions)]
println!("Debug: {:?}", data);
```

### Getting Help

- **Issues**: [GitHub Issues](https://github.com/Sucotasch/HotkeyImageSorter/issues)
- **Discussions**: [GitHub Discussions](https://github.com/Sucotasch/HotkeyImageSorter/discussions)
- **Documentation**: This README + Wiki

---

## License

This project is licensed under the MIT License - see the LICENSE file for details.

## Contributing

Contributions are welcome! Please read our contributing guidelines before submitting PRs.

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Submit a pull request

---

## Acknowledgments

- [Tauri](https://tauri.app/) - Desktop application framework
- [Rust](https://www.rust-lang.org/) - Systems programming language
- Community contributors and users

---

<div align="center">

**Made with ❤️ using Tauri + Rust**

[Report Bug](https://github.com/Sucotasch/HotkeyImageSorter/issues) · [Request Feature](https://github.com/Sucotasch/HotkeyImageSorter/issues)

</div>

---

---

# HotkeyImageSorter

> **Минималистичное приложение для быстрого просмотра/редактирования/перемещения изображений в целевые директории с пользовательскими горячими клавишами**

---

## Содержание

1. [Обзор](#обзор)
2. [Возможности](#возможности)
3. [Архитектура](#архитектура)
4. [Установка](#установка)
5. [Конфигурация](#конфигурация)
6. [Руководство по использованию](#руководство-по-использованию)
7. [Горячие клавиши](#горячие-клавиши)
8. [Технические детали](#технические-детали)
9. [Разработка](#разработка)
10. [Устранение неполадок](#устранение-неполадок)

---

## Обзор

**HotkeyImageSorter** — это настольное приложение, созданное с использованием [Tauri](https://tauri.app/), которое обеспечивает быструю организацию изображений через управление с клавиатуры. Приложение разработано для фотографов, дизайнеров и всех, кому нужно эффективно сортировать большие пакеты изображений, сочетая быстрый просмотр с организацией файлов одним нажатием клавиши.

### Ключевые преимущества

- ⚡ **Скорость**: Навигация и сортировка изображений без использования мыши
- 🎯 **Точность**: Пользовательские горячие клавиши для каждой целевой директории
- 🔄 **Безопасность**: Функции отмены/повтора для операций перемещения
- 🖼️ **Предпросмотр**: Встроенный просмотрщик изображений с масштабированием, поворотом и обрезкой

---

## Возможности

### Основной функционал

| Возможность | Описание |
|-------------|----------|
| **Сортировка горячими клавишами** | Назначение директорий на клавиши A-Z для мгновенной организации файлов |
| **Предпросмотр изображений** | Высококачественное отображение с метаданными |
| **Пакетная навигация** | Навигация по всем файлам папки с помощью клавиатуры |
| **Отмена/Повтор** | Отмена случайных перемещений с полным отслеживанием истории |
| **Редактирование изображений** | Поворот (90°), обрезка и масштабирование |
| **Умная сортировка** | Сортировка по имени, дате или размеру с порядком возрастания/убывания |
| **Сохранение конфигурации** | Настройки сохраняются между сеансами (папки, горячие клавиши, предпочтения) |

### Поддерживаемые форматы изображений

Приложение поддерживает все стандартные форматы изображений:
- JPEG/JPG
- PNG
- GIF
- BMP
- WebP
- TIFF

---

## Архитектура

### Технологический стек

```
┌─────────────────────────────────────────────────────────────┐
│                      Frontend (JavaScript)                   │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │   app.js    │  │  index.html │  │   style.css         │  │
│  │ (Логика UI) │  │ (Структура) │  │   (Стили)           │  │
│  └─────────────┘  └─────────────┘  └─────────────────────┘  │
│                         Tauri API Bridge                      │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                       Backend (Rust)                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │ file_ops.rs │  │image_proc.rs│  │   config.rs         │  │
│  │(Операции с  │  │ (Редактиро- │  │  (Настройки)        │  │
│  │  файлами)   │  │  вание)     │  │                     │  │
│  └─────────────┘  └─────────────┘  └─────────────────────┘  │
│                      Tauri Core Runtime                      │
└─────────────────────────────────────────────────────────────┘
```

### Компоненты

#### Frontend (`src/`)
| Файл | Назначение |
|------|------------|
| `app.js` | Основная логика приложения, обработчики событий, управление состоянием |
| `index.html` | Структура приложения и элементы UI |
| `style.css` / `styles.css` | Визуальное оформление и адаптивный дизайн |
| `main.js` | Точка входа и инициализация приложения |

#### Backend (`src-tauri/src/`)
| Файл | Назначение |
|------|------------|
| `main.rs` | Точка входа приложения Tauri |
| `lib.rs` | Экспорт библиотеки и регистрация команд |
| `file_ops.rs` | Операции с файловой системой (перемещение, список, удаление) |
| `image_proc.rs` | Обработка изображений (поворот, обрезка, метаданные) |
| `config.rs` | Управление конфигурацией и сохранение |
| `models.rs` | Структуры данных и определения типов |

### Поток данных

```
Действие пользователя → Событие Frontend → Tauri Invoke → Rust Команда → Файловая система
     ↑                                                              │
     └────────────────────── Ответ ─────────────────────────────┘
```

---

## Установка

### Требования

Перед установкой убедитесь, что у вас есть следующее:

| Требование | Версия | Установка |
|------------|--------|-----------|
| Node.js | 18+ | [nodejs.org](https://nodejs.org/) |
| Rust | 1.70+ | [rustup.rs](https://rustup.rs/) |
| Git | Последняя | [git-scm.com](https://git-scm.com/) |

### Требования для платформ

#### Windows
```powershell
# Установите WebView2 (обычно предустановлен на Windows 10+)
# Установите Visual Studio Build Tools с рабочей нагрузкой C++
```

#### macOS
```bash
# Установите Xcode Command Line Tools
xcode-select --install
```

#### Linux
```bash
# Ubuntu/Debian
sudo apt update
sudo apt install libwebkit2gtk-4.0-dev build-essential curl wget \
    libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev

# Fedora
sudo dnf install webkit2gtk3-devel openssl-devel curl wget \
    gtk3-devel libappindicator-gtk3-devel librsvg2-devel
```

### Шаги установки

```bash
# 1. Клонируйте репозиторий
git clone https://github.com/Sucotasch/HotkeyImageSorter.git
cd HotkeyImageSorter

# 2. Установите зависимости Node.js
npm install

# 3. Запустите в режиме разработки
npm run dev

# 4. Соберите для производства
npm run build
```

### Готовые бинарные файлы

Загрузите предварительно скомпилированные бинарные файлы со страницы [Releases](https://github.com/Sucotasch/HotkeyImageSorter/releases):
- **Windows**: `.exe` установщик или портативный `.zip`
- **macOS**: `.dmg` или `.app` bundle
- **Linux**: `.deb`, `.rpm` или `.AppImage`

---

## Конфигурация

### Начальная настройка

1. **Запустите приложение**
2. **Выберите исходную папку**: Нажмите "Select Folder" или `Ctrl+O`
3. **Настройте горячие клавиши**: Откройте настройки (`Ctrl+,`) для назначения директорий на клавиши

### Файл конфигурации

Настройки хранятся в JSON файле (расположение зависит от ОС):

| ОС | Путь к конфигурации |
|----|---------------------|
| Windows | `%APPDATA%\HotkeyImageSorter\config.json` |
| macOS | `~/Library/Application Support/HotkeyImageSorter/config.json` |
| Linux | `~/.config/HotkeyImageSorter/config.json` |

### Схема конфигурации

```json
{
  "last_folder": "/путь/к/исходным/изображениям",
  "last_file": "/путь/к/последнему/просмотренному/изображению.jpg",
  "sort_by": "name",
  "sort_dir": 1,
  "hotkeys": {
    "a": "/путь/к/категории/a",
    "b": "/путь/к/категории/b",
    "c": "/путь/к/категории/c"
  }
}
```

### Конфигурация горячих клавиш

| Клавиша | Действие по умолчанию | Настраиваемая |
|---------|----------------------|---------------|
| A-Z | Пользовательская директория | ✅ Да |
| 0-9 | Зарезервировано | ❌ Нет |
| Специальные | Системные сокращения | ❌ Нет |

---

## Руководство по использованию

### Быстрый старт

```
1. Откройте приложение
       ↓
2. Выберите исходную папку (Ctrl+O)
       ↓
3. Навигация по изображениям (← → или стрелки)
       ↓
4. Нажмите горячую клавишу (A-Z) для сортировки
       ↓
5. Продолжите или отмените (Ctrl+Z) при необходимости
```

### Основные функции

#### 1. Выбор папки

**Способ 1**: Нажмите кнопку "Select Folder"
**Способ 2**: Нажмите `Ctrl+O`

```javascript
// Внутри
async function selectFolder() {
  const selected = await open({
    directory: true,
    multiple: false,
  });
  // Загружает все изображения из выбранной директории
}
```

#### 2. Навигация по изображениям

| Действие | Горячая клавиша |
|----------|-----------------|
| Следующее изображение | `→` или `L` |
| Предыдущее изображение | `←` или `H` |
| Первое изображение | `Home` |
| Последнее изображение | `End` |

#### 3. Сортировка изображений

1. Настройте целевые директории в настройках
2. Просмотрите изображение для сортировки
3. Нажмите назначенную горячую клавишу (A-Z)
4. Изображение автоматически перемещается в целевую директорию
5. Автоматически загружается следующее изображение

#### 4. Редактирование изображений

##### Поворот
```
Ctrl+R или Ctrl+Shift+R
```
Поворачивает изображение на 90° по часовой стрелке. Изменения сохраняются в исходный файл.

##### Обрезка
```
1. Нажмите Ctrl+C для активации режима обрезки
2. Перетащите область обрезки в нужное место
3. Двойной клик для выполнения обрезки
4. Нажмите Ctrl+C снова для отмены
```

##### Масштабирование
```
Колесо мыши: Увеличение/уменьшение
Ctrl+0: Сброс масштаба до 100%
```

#### 5. Отмена/Повтор операций

| Действие | Горячая клавиша | Описание |
|----------|-----------------|----------|
| Отмена | `Ctrl+Z` | Отменить последнее перемещение |
| Повтор | `Ctrl+X` | Повторить отменённое перемещение |

```javascript
// Реализация отмены
async function doUndo() {
  const undonePath = await invoke('undo_last_move');
  if (undonePath) {
    await loadFiles();
    refreshCurrentImage();
  }
}
```

#### 6. Удаление изображения

```
Delete или Backspace
```
Перемещает изображение в корзину системы (настраивается в настройках).

---

## Горячие клавиши

### Навигация

| Горячая клавиша | Действие |
|-----------------|----------|
| `←` / `H` | Предыдущее изображение |
| `→` / `L` | Следующее изображение |
| `Home` | Первое изображение |
| `End` | Последнее изображение |
| `Ctrl+O` | Открыть папку |

### Сортировка

| Горячая клавиша | Действие |
|-----------------|----------|
| `A` - `Z` | Переместить в назначенную директорию |
| `Delete` | Удалить текущее изображение |

### Редактирование

| Горячая клавиша | Действие |
|-----------------|----------|
| `Ctrl+R` | Поворот на 90° по часовой стрелке |
| `Ctrl+C` | Переключить режим обрезки |
| `Колесо мыши` | Масштабирование |
| `Ctrl+0` | Сброс масштаба |

### Система

| Горячая клавиша | Действие |
|-----------------|----------|
| `Ctrl+Z` | Отменить последнее перемещение |
| `Ctrl+X` | Повторить перемещение |
| `Ctrl+,` | Открыть настройки |
| `Esc` | Закрыть модальное окно/диалог |

---

## Технические детали

### Управление состоянием

Приложение хранит состояние в глобальном объекте:

```javascript
const state = {
  folder: null,           // Путь к исходной директории
  images: [],             // Массив метаданных изображений
  currentIndex: 0,        // Текущий индекс изображения
  currentPath: '',        // Полный путь к текущему изображению
  config: {},             // Пользовательская конфигурация
  sort: { by: 'name', dir: 1 },
  isCropActive: false,
  history: { undo: [], redo: [] }
};
```

### Конвейер загрузки изображений

```
Путь к файлу → Tauri convertFileSrc → Blob URL → Элемент Image → Рендеринг
                ↓
         Cache Buster (?t=timestamp)
                ↓
           Принудительное обновление
```

### Операции с файлами

Все операции с файлами выполняются через команды Tauri:

| Команда | Параметры | Возвращает |
|---------|-----------|------------|
| `list_files` | `{ dir: string }` | `ImageInfo[]` |
| `move_image` | `{ source, targetDir }` | `newPath` |
| `rotate_image` | `{ path, degrees }` | `void` |
| `undo_last_move` | `{}` | `undonePath` |
| `redo_last_move` | `{}` | `redonePath` |
| `save_config` | `{ config }` | `void` |
| `get_history_stats` | `{}` | `[undoCount, redoCount]` |

### Производительность

- **Ленивая загрузка**: Изображения загружаются по требованию
- **Кэширование метаданных**: Информация о файлах кэшируется после первоначального сканирования
- **Эффективный рендеринг**: Обновляются только видимые элементы
- **Фоновые операции**: Перемещение файлов не блокирует UI

---

## Разработка

### Структура проекта

```
HotkeyImageSorter/
├── src/                    # Исходный код frontend
│   ├── app.js             # Основная логика приложения
│   ├── main.js            # Точка входа
│   ├── index.html         # HTML структура
│   └── style.css          # Стили
├── src-tauri/             # Rust backend
│   ├── src/
│   │   ├── main.rs        # Точка входа приложения
│   │   ├── lib.rs         # Экспорт команд
│   │   ├── file_ops.rs    # Операции с файлами
│   │   ├── image_proc.rs  # Обработка изображений
│   │   ├── config.rs      # Конфигурация
│   │   └── models.rs      # Модели данных
│   ├── Cargo.toml         # Rust зависимости
│   └── tauri.conf.json    # Конфигурация Tauri
├── package.json           # Node.js зависимости
└── README.md              # Этот файл
```

### Команды разработки

```bash
# Запуск сервера разработки
npm run dev

# Сборка для производства
npm run build

# Сборка для конкретной платформы
npm run tauri build -- --target x86_64-pc-windows-msvc
```

### Добавление новых функций

1. **Frontend**: Добавьте элементы UI в `index.html`, логику в `app.js`
2. **Backend**: Создайте Rust функцию в соответствующем модуле
3. **Мост**: Зарегистрируйте команду в `lib.rs`
4. **Вызов**: Вызовите из frontend используя `invoke('имя_команды', параметры)`

### Тестирование

```bash
# Запуск Rust тестов
cd src-tauri && cargo test

# Запуск frontend тестов (если настроено)
npm test
```

---

## Устранение неполадок

### Распространённые проблемы

#### Изображения не загружаются
```
Решение: Проверьте права доступа к файлам и доступность пути
```

#### Горячие клавиши не работают
```
Решение: 
1. Проверьте, что горячая клавиша настроена в настройках
2. Убедитесь, что целевая директория существует
3. Проверьте права на запись для целевой директории
```

#### Приложение падает при запуске
```
Решение:
1. Удалите файл конфигурации
2. Переустановите приложение
3. Проверьте системные требования
```

#### Отмена/Повтор не работают
```
Решение: История хранится в течение сеанса. Перезапуск очищает историю.
```

### Режим отладки

Включите логирование отладки:

```javascript
// В app.js
console.log('Отладка:', state);
```

```rust
// В Rust backend
#[cfg(debug_assertions)]
println!("Отладка: {:?}", data);
```

### Получение помощи

- **Проблемы**: [GitHub Issues](https://github.com/Sucotasch/HotkeyImageSorter/issues)
- **Обсуждения**: [GitHub Discussions](https://github.com/Sucotasch/HotkeyImageSorter/discussions)
- **Документация**: Этот README + Wiki

---

## Лицензия

Этот проект лицензирован под лицензией MIT — см. файл LICENSE для деталей.

## Вклад в проект

Вклад приветствуется! Пожалуйста, прочитайте наши руководства по вкладу перед отправкой PR.

1. Форкните репозиторий
2. Создайте ветку функции
3. Внесите изменения
4. Отправьте pull request

---

## Благодарности

- [Tauri](https://tauri.app/) - Фреймворк для настольных приложений
- [Rust](https://www.rust-lang.org/) - Язык системного программирования
- Участники сообщества и пользователи

---

<div align="center">

**Сделано с ❤️ используя Tauri + Rust**

[Сообщить об ошибке](https://github.com/Sucotasch/HotkeyImageSorter/issues) · [Запросить функцию](https://github.com/Sucotasch/HotkeyImageSorter/issues)

</div>
