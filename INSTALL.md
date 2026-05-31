# AII Blockchain — Multilingual Installation Guide

> Select your language / 选择语言 / 言語を選択 / 언어 선택

| 🇺🇸 [English](#-english) | 🇨🇳 [简体中文](#-简体中文) | 🇯🇵 [日本語](#-日本語) | 🇰🇷 [한국어](#-한국어) | 🇷🇺 [Русский](#-русский) |
|---|---|---|---|---|
| 🇩🇪 [Deutsch](#-deutsch) | 🇫🇷 [Français](#-français) | 🇧🇷 [Português](#-português) | 🇮🇳 [हिन्दी](#-हिन्दी) | 🇸🇦 [العربية](#-العربية) |

---

## 🇺🇸 English

### Prerequisites

- **Rust** 1.85 or later
- **Git** 2.x
- **Linux / macOS / Windows (WSL2)**
- 4 GB RAM minimum, 8 GB recommended
- 20 GB disk space for testnet data

### Step 1 — Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
rustup update stable
rustup --version   # rustup 1.27+
```

### Step 2 — Clone & Build

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii
cargo build --release -p aii-node -p aii-cli -p aii-mcp
```

> Build time: ~5–10 minutes on first run (compiles RocksDB from source).

### Step 3 — Verify Installation

```bash
./target/release/aiid --version   # aiid 0.0.93
./target/release/aii  --version   # aii  0.0.93

# Test connectivity to live testnet
./target/release/aii --rpc https://aii.allfund.xyz/api status
```

### Step 4 — Connect to Testnet (Observer Node)

```bash
mkdir -p ~/aii-data
./target/release/aiid \
  --data-dir ~/aii-data \
  --rpc 127.0.0.1:8545 \
  --produce-blocks false \
  --bootnode http://8.211.135.234:8545 \
  --testnet
```

### Step 5 — Add to MetaMask

| Field | Value |
|-------|-------|
| Network Name | AII Testnet |
| RPC URL | `https://aii.allfund.xyz/api` |
| Chain ID | `9999` |
| Currency | `AII` |
| Explorer | `https://aii.allfund.xyz/` |

### Troubleshooting

```bash
# Linker error on Ubuntu
sudo apt-get install -y build-essential clang libclang-dev

# Port already in use
lsof -i :8545   # find the process
kill -9 <PID>

# Reset node data
rm -rf ~/aii-data && mkdir -p ~/aii-data
```

---

## 🇨🇳 简体中文

### 前置要求

- **Rust** 1.85 或更高版本
- **Git** 2.x
- **Linux / macOS / Windows（WSL2）**
- 最低 4 GB 内存，推荐 8 GB
- 20 GB 磁盘空间（测试网数据）

### 第一步 — 安装 Rust

```bash
# 推荐使用国内镜像加速
export RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static
export RUSTUP_UPDATE_ROOT=https://mirrors.ustc.edu.cn/rust-static/rustup
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
rustup update stable
```

> 💡 国内用户可使用中科大或字节跳动镜像，显著提升下载速度。

### 第二步 — 克隆并编译

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii

# 可选：配置 crates.io 国内镜像（加速 crate 下载）
cat >> ~/.cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = "ustc"
[source.ustc]
registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/"
EOF

cargo build --release -p aii-node -p aii-cli -p aii-mcp
```

### 第三步 — 验证安装

```bash
./target/release/aiid --version   # aiid 0.0.93
./target/release/aii  --version   # aii  0.0.93

# 测试与测试网的连接
./target/release/aii --rpc http://106.14.223.128:8545 status
```

### 第四步 — 连接测试网（观察节点）

```bash
mkdir -p ~/aii-data
./target/release/aiid \
  --data-dir ~/aii-data \
  --rpc 127.0.0.1:8545 \
  --produce-blocks false \
  --bootnode http://106.14.223.128:8545 \
  --testnet
```

### 第五步 — MetaMask 配置

| 字段 | 值 |
|------|----|
| 网络名称 | AII 测试网 |
| RPC URL | `https://aii.allfund.xyz/api` |
| 链 ID | `9999` |
| 货币符号 | `AII` |
| 区块浏览器 | `https://aii.allfund.xyz/` |

### 常见问题

```bash
# Ubuntu 编译报链接错误
sudo apt-get install -y build-essential clang libclang-dev

# 端口被占用
lsof -i :8545
kill -9 <PID>

# 重置节点数据
rm -rf ~/aii-data && mkdir -p ~/aii-data
```

---

## 🇯🇵 日本語

### 前提条件

- **Rust** 1.85 以上
- **Git** 2.x
- **Linux / macOS / Windows（WSL2）**
- 最低 4 GB RAM、推奨 8 GB
- 20 GB のディスク容量（テストネットデータ）

### ステップ 1 — Rust のインストール

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
rustup update stable
rustc --version   # rustc 1.85.0 以上
```

### ステップ 2 — クローンとビルド

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii
cargo build --release -p aii-node -p aii-cli -p aii-mcp
```

> ⏱️ 初回ビルドには 5〜10 分かかります（RocksDB をソースからコンパイルします）。

### ステップ 3 — インストールの確認

```bash
./target/release/aiid --version   # aiid 0.0.93
./target/release/aii  --version   # aii  0.0.93

# テストネットへの接続確認
./target/release/aii --rpc http://8.211.135.234:8545 status
```

### ステップ 4 — テストネットへの接続（オブザーバーノード）

```bash
mkdir -p ~/aii-data
./target/release/aiid \
  --data-dir ~/aii-data \
  --rpc 127.0.0.1:8545 \
  --produce-blocks false \
  --bootnode http://8.211.135.234:8545 \
  --testnet
```

### ステップ 5 — MetaMask の設定

| フィールド | 値 |
|-----------|-----|
| ネットワーク名 | AII テストネット |
| RPC URL | `https://aii.allfund.xyz/api` |
| チェーン ID | `9999` |
| 通貨シンボル | `AII` |
| ブロックエクスプローラー | `https://aii.allfund.xyz/` |

### トラブルシューティング

```bash
# Ubuntu でリンカーエラーが発生した場合
sudo apt-get install -y build-essential clang libclang-dev

# ポートが使用中の場合
lsof -i :8545 && kill -9 <PID>

# ノードデータのリセット
rm -rf ~/aii-data && mkdir -p ~/aii-data
```

---

## 🇰🇷 한국어

### 사전 요구 사항

- **Rust** 1.85 이상
- **Git** 2.x
- **Linux / macOS / Windows (WSL2)**
- 최소 4 GB RAM, 8 GB 권장
- 20 GB 디스크 공간 (테스트넷 데이터)

### 1단계 — Rust 설치

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
rustup update stable
rustc --version   # rustc 1.85.0 이상
```

### 2단계 — 클론 및 빌드

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii
cargo build --release -p aii-node -p aii-cli -p aii-mcp
```

> 💡 첫 빌드 시 RocksDB 컴파일로 인해 5~10분 소요됩니다.

### 3단계 — 설치 확인

```bash
./target/release/aiid --version   # aiid 0.0.93
./target/release/aii  --version   # aii  0.0.93

# 테스트넷 연결 확인
./target/release/aii --rpc https://aii.allfund.xyz/api status
```

### 4단계 — 테스트넷 연결 (관찰자 노드)

```bash
mkdir -p ~/aii-data
./target/release/aiid \
  --data-dir ~/aii-data \
  --rpc 127.0.0.1:8545 \
  --produce-blocks false \
  --bootnode http://8.211.135.234:8545 \
  --testnet
```

### 5단계 — MetaMask 설정

| 필드 | 값 |
|------|----|
| 네트워크 이름 | AII 테스트넷 |
| RPC URL | `https://aii.allfund.xyz/api` |
| 체인 ID | `9999` |
| 통화 기호 | `AII` |
| 블록 탐색기 | `https://aii.allfund.xyz/` |

---

## 🇷🇺 Русский

### Предварительные требования

- **Rust** 1.85 или выше
- **Git** 2.x
- **Linux / macOS / Windows (WSL2)**
- Минимум 4 ГБ ОЗУ, рекомендуется 8 ГБ
- 20 ГБ дискового пространства

### Шаг 1 — Установка Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
rustup update stable
rustc --version   # rustc 1.85.0 и выше
```

### Шаг 2 — Клонирование и сборка

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii
cargo build --release -p aii-node -p aii-cli -p aii-mcp
```

> ⏱️ Первая сборка занимает 5–10 минут (компиляция RocksDB из исходников).

### Шаг 3 — Проверка установки

```bash
./target/release/aiid --version   # aiid 0.0.93
./target/release/aii  --version   # aii  0.0.93

# Проверка подключения к тестовой сети
./target/release/aii --rpc https://aii.allfund.xyz/api status
```

### Шаг 4 — Подключение к тестовой сети (узел-наблюдатель)

```bash
mkdir -p ~/aii-data
./target/release/aiid \
  --data-dir ~/aii-data \
  --rpc 127.0.0.1:8545 \
  --produce-blocks false \
  --bootnode http://8.211.135.234:8545 \
  --testnet
```

### Шаг 5 — Настройка MetaMask

| Поле | Значение |
|------|----------|
| Название сети | AII Тестовая сеть |
| URL RPC | `https://aii.allfund.xyz/api` |
| ID цепочки | `9999` |
| Символ валюты | `AII` |
| Обозреватель блоков | `https://aii.allfund.xyz/` |

### Устранение неполадок

```bash
# Ошибка компоновщика в Ubuntu
sudo apt-get install -y build-essential clang libclang-dev

# Порт занят
lsof -i :8545 && kill -9 <PID>

# Сброс данных узла
rm -rf ~/aii-data && mkdir -p ~/aii-data
```

---

## 🇩🇪 Deutsch

### Voraussetzungen

- **Rust** 1.85 oder höher
- **Git** 2.x
- **Linux / macOS / Windows (WSL2)**
- Mindestens 4 GB RAM, 8 GB empfohlen
- 20 GB Festplattenspeicher

### Schritt 1 — Rust installieren

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
rustup update stable
rustc --version   # rustc 1.85.0 oder neuer
```

### Schritt 2 — Klonen und Kompilieren

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii
cargo build --release -p aii-node -p aii-cli -p aii-mcp
```

> ⏱️ Der erste Build dauert 5–10 Minuten (kompiliert RocksDB aus den Quellen).

### Schritt 3 — Installation prüfen

```bash
./target/release/aiid --version   # aiid 0.0.93
./target/release/aii  --version   # aii  0.0.93

# Verbindung zum Testnetz prüfen
./target/release/aii --rpc https://aii.allfund.xyz/api status
```

### Schritt 4 — Mit Testnetz verbinden (Beobachterknoten)

```bash
mkdir -p ~/aii-data
./target/release/aiid \
  --data-dir ~/aii-data \
  --rpc 127.0.0.1:8545 \
  --produce-blocks false \
  --bootnode http://8.211.135.234:8545 \
  --testnet
```

### Schritt 5 — MetaMask-Konfiguration

| Feld | Wert |
|------|------|
| Netzwerkname | AII Testnetz |
| RPC-URL | `https://aii.allfund.xyz/api` |
| Chain-ID | `9999` |
| Währungssymbol | `AII` |
| Block-Explorer | `https://aii.allfund.xyz/` |

---

## 🇫🇷 Français

### Prérequis

- **Rust** 1.85 ou supérieur
- **Git** 2.x
- **Linux / macOS / Windows (WSL2)**
- 4 Go de RAM minimum, 8 Go recommandé
- 20 Go d'espace disque

### Étape 1 — Installer Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
rustup update stable
rustc --version   # rustc 1.85.0 ou supérieur
```

### Étape 2 — Cloner et compiler

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii
cargo build --release -p aii-node -p aii-cli -p aii-mcp
```

> ⏱️ La première compilation prend 5 à 10 minutes (compilation de RocksDB depuis les sources).

### Étape 3 — Vérifier l'installation

```bash
./target/release/aiid --version   # aiid 0.0.93
./target/release/aii  --version   # aii  0.0.93

# Tester la connexion au réseau de test
./target/release/aii --rpc https://aii.allfund.xyz/api status
```

### Étape 4 — Se connecter au réseau de test (nœud observateur)

```bash
mkdir -p ~/aii-data
./target/release/aiid \
  --data-dir ~/aii-data \
  --rpc 127.0.0.1:8545 \
  --produce-blocks false \
  --bootnode http://8.211.135.234:8545 \
  --testnet
```

### Étape 5 — Configuration MetaMask

| Champ | Valeur |
|-------|--------|
| Nom du réseau | AII Réseau de test |
| URL RPC | `https://aii.allfund.xyz/api` |
| ID de chaîne | `9999` |
| Symbole monétaire | `AII` |
| Explorateur de blocs | `https://aii.allfund.xyz/` |

---

## 🇧🇷 Português

### Pré-requisitos

- **Rust** 1.85 ou superior
- **Git** 2.x
- **Linux / macOS / Windows (WSL2)**
- Mínimo 4 GB RAM, 8 GB recomendado
- 20 GB de espaço em disco

### Passo 1 — Instalar o Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
rustup update stable
rustc --version   # rustc 1.85.0 ou superior
```

### Passo 2 — Clonar e Compilar

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii
cargo build --release -p aii-node -p aii-cli -p aii-mcp
```

> ⏱️ A primeira compilação leva 5–10 minutos (compila o RocksDB do código-fonte).

### Passo 3 — Verificar a Instalação

```bash
./target/release/aiid --version   # aiid 0.0.93
./target/release/aii  --version   # aii  0.0.93

# Testar conectividade com a rede de teste
./target/release/aii --rpc https://aii.allfund.xyz/api status
```

### Passo 4 — Conectar à Rede de Teste (Nó Observador)

```bash
mkdir -p ~/aii-data
./target/release/aiid \
  --data-dir ~/aii-data \
  --rpc 127.0.0.1:8545 \
  --produce-blocks false \
  --bootnode http://8.211.135.234:8545 \
  --testnet
```

### Passo 5 — Configuração do MetaMask

| Campo | Valor |
|-------|-------|
| Nome da Rede | AII Rede de Teste |
| URL RPC | `https://aii.allfund.xyz/api` |
| ID da Cadeia | `9999` |
| Símbolo da Moeda | `AII` |
| Explorador de Blocos | `https://aii.allfund.xyz/` |

---

## 🇮🇳 हिन्दी

### पूर्व-आवश्यकताएँ

- **Rust** 1.85 या उससे अधिक
- **Git** 2.x
- **Linux / macOS / Windows (WSL2)**
- न्यूनतम 4 GB RAM, अनुशंसित 8 GB
- 20 GB डिस्क स्थान

### चरण 1 — Rust स्थापित करें

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
rustup update stable
rustc --version   # rustc 1.85.0 या उससे अधिक
```

### चरण 2 — क्लोन और बिल्ड करें

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii
cargo build --release -p aii-node -p aii-cli -p aii-mcp
```

> ⏱️ पहली बार बिल्ड में 5–10 मिनट लगते हैं (RocksDB को सोर्स से कंपाइल करता है)।

### चरण 3 — स्थापना सत्यापित करें

```bash
./target/release/aiid --version   # aiid 0.0.93
./target/release/aii  --version   # aii  0.0.93

# टेस्टनेट कनेक्टिविटी परीक्षण
./target/release/aii --rpc https://aii.allfund.xyz/api status
```

### चरण 4 — टेस्टनेट से कनेक्ट करें (ऑब्जर्वर नोड)

```bash
mkdir -p ~/aii-data
./target/release/aiid \
  --data-dir ~/aii-data \
  --rpc 127.0.0.1:8545 \
  --produce-blocks false \
  --bootnode http://8.211.135.234:8545 \
  --testnet
```

### चरण 5 — MetaMask कॉन्फ़िगरेशन

| फ़ील्ड | मान |
|--------|-----|
| नेटवर्क नाम | AII टेस्टनेट |
| RPC URL | `https://aii.allfund.xyz/api` |
| चेन ID | `9999` |
| मुद्रा प्रतीक | `AII` |
| ब्लॉक एक्सप्लोरर | `https://aii.allfund.xyz/` |

---

## 🇸🇦 العربية

<div dir="rtl">

### المتطلبات المسبقة

- **Rust** الإصدار 1.85 أو أحدث
- **Git** الإصدار 2.x
- **Linux / macOS / Windows (WSL2)**
- ذاكرة وصول عشوائي 4 جيجابايت كحد أدنى، يُوصى بـ 8 جيجابايت
- 20 جيجابايت من مساحة القرص

### الخطوة 1 — تثبيت Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
rustup update stable
rustc --version   # rustc 1.85.0 أو أحدث
```

### الخطوة 2 — الاستنساخ والبناء

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii
cargo build --release -p aii-node -p aii-cli -p aii-mcp
```

> ⏱️ يستغرق أول بناء من 5 إلى 10 دقائق (يُترجم RocksDB من المصدر).

### الخطوة 3 — التحقق من التثبيت

```bash
./target/release/aiid --version   # aiid 0.0.93
./target/release/aii  --version   # aii  0.0.93

# اختبار الاتصال بالشبكة التجريبية
./target/release/aii --rpc https://aii.allfund.xyz/api status
```

### الخطوة 4 — الاتصال بالشبكة التجريبية (عقدة المراقب)

```bash
mkdir -p ~/aii-data
./target/release/aiid \
  --data-dir ~/aii-data \
  --rpc 127.0.0.1:8545 \
  --produce-blocks false \
  --bootnode http://8.211.135.234:8545 \
  --testnet
```

### الخطوة 5 — إعداد MetaMask

| الحقل | القيمة |
|-------|--------|
| اسم الشبكة | AII شبكة الاختبار |
| عنوان RPC | `https://aii.allfund.xyz/api` |
| معرّف السلسلة | `9999` |
| رمز العملة | `AII` |
| مستكشف الكتل | `https://aii.allfund.xyz/` |

### استكشاف الأخطاء وإصلاحها

```bash
# خطأ في الرابط على Ubuntu
sudo apt-get install -y build-essential clang libclang-dev

# المنفذ مستخدم
lsof -i :8545 && kill -9 <PID>

# إعادة تعيين بيانات العقدة
rm -rf ~/aii-data && mkdir -p ~/aii-data
```

</div>

---

<div align="center">

**[⬆ Back to top](#aii-blockchain--multilingual-installation-guide)**

[![README](https://img.shields.io/badge/←_Back_to-README.md-blue?style=for-the-badge)](README.md)

</div>
