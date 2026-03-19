# 📦 @nijorjs/nparser

A fast, lightweight parser for `.nijor` files — built in Rust.

---

## 🚀 Why nparser?

Nijor originally used `jsdom` to parse `.nijor` files. While it works for standard HTML, it introduces several issues for Nijor’s syntax:

- 🐢 **Slow performance** due to full DOM emulation  
- 📦 **Heavy dependency** footprint  
- ⚠️ **Unwanted transformations**
  - `<component attr>` → `<component attr="">`
  - Escaping of characters like `<`, `>`, `&`

`.nijor` files are **HTML-like, not HTML**, and these transformations break expected behavior.

---

## 💡 The Solution

`nparser` is a **custom parser written in Rust**, designed specifically for `.nijor` syntax.

It avoids HTML assumptions and preserves the original structure exactly as written.

---

## ✨ Features

- ⚡ Blazing fast (Rust-powered)
- 🎯 No unwanted HTML normalization
- 🧠 Understands Nijor-specific syntax
- 🪶 Lightweight and efficient
- 🔒 Memory-safe (thanks to Rust)

---

## 📦 Installation

```bash
npm install @nijor/nparser