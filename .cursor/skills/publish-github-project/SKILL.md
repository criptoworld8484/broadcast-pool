---
name: publish-github-project
description: >-
  Guides ordered publication of a local project to GitHub: pre-flight audit,
  .gitignore and secrets check, README/LICENSE, remote setup, first push, repo
  metadata, and optional release. Use when the user wants to publish, push, or
  open-source a project on GitHub, create a remote repository, or prepare a
  repo for public release.
---

# Publicar proyecto en GitHub

Ejecuta este flujo en orden. No saltes pasos de seguridad ni hagas push sin confirmación explícita del usuario si hay dudas sobre secretos.

## Requisitos previos

- `git` instalado y repositorio inicializado (`git init` si hace falta).
- `gh` autenticado: `gh auth status` (si falla, pedir `gh auth login`).
- El usuario debe haber pedido explícitamente **commit** y/o **push** antes de ejecutarlos.

## Checklist de progreso

Copia y actualiza en la respuesta:

```
Publicación GitHub:
- [ ] 1. Auditoría pre-publicación
- [ ] 2. Archivos de repositorio (README, LICENSE, .gitignore)
- [ ] 3. Estado git limpio y commit inicial
- [ ] 4. Repositorio remoto en GitHub
- [ ] 5. Push y verificación
- [ ] 6. Metadatos y opcional release
```

---

## Paso 1: Auditoría pre-publicación

En paralelo cuando sea posible:

```bash
git status
git remote -v
git log --oneline -5 2>/dev/null || true
```

Comprobar manualmente:

| Riesgo | Acción |
|--------|--------|
| `.env`, claves, wallets, `*.pem`, credenciales | Añadir a `.gitignore`; **nunca** commitear |
| `target/`, `node_modules/`, `dist/`, `.DS_Store` | Deben estar en `.gitignore` |
| Binarios grandes o datos personales | Excluir o usar Git LFS solo si el usuario lo pide |
| Historial con secretos ya commiteados | Avisar; requiere rotación de claves y limpieza de historial (no `filter-branch` sin acuerdo) |

Para Rust: confirmar `/target` en `.gitignore`.

---

## Paso 2: Archivos de repositorio

Crear o completar solo lo que falte (respetar convenciones del proyecto):

- **README.md**: nombre, descripción, requisitos, build/run, configuración (sin secretos), licencia.
- **LICENSE**: alinear con `license` en `Cargo.toml` / `package.json` si existe.
- **.gitignore**: plantilla del stack (Rust, Node, Python, etc.).

No inventar URLs ni nombres de repo; preguntar si no están claros.

---

## Paso 3: Commit inicial o de preparación

Solo si el usuario pidió commit:

1. `git status`, `git diff`, `git log` (estilo de mensajes del repo).
2. Excluir archivos sensibles del stage.
3. Commit con HEREDOC, mensaje en 1–2 frases centrado en el *por qué*.
4. No usar `--no-verify`, amend ni force push salvo petición explícita.

---

## Paso 4: Repositorio remoto

Decidir con el usuario:

- **Repo nuevo** (nombre, público/privado, organización vs personal).
- **Repo existente** (URL del remoto).

### Crear repo nuevo con GitHub CLI

```bash
# Público (ajustar --private si hace falta)
gh repo create NOMBRE_REPO --source=. --remote=origin --description="DESCRIPCION" --public

# Si el remoto ya existe localmente:
git remote add origin https://github.com/USUARIO/NOMBRE_REPO.git
# o SSH: git@github.com:USUARIO/NOMBRE_REPO.git
```

Verificar: `git remote -v`.

---

## Paso 5: Push y verificación

Solo con permiso explícito del usuario:

```bash
git branch -M main   # o master si el proyecto ya usa master
git push -u origin HEAD
```

Comprobar:

```bash
gh repo view --web   # opcional, abre en navegador
git status
```

Si el push falla (auth, rama protegida, historial no relacionado): diagnosticar y proponer una solución; no hacer force push a `main`/`master` sin aviso.

---

## Paso 6: Metadatos en GitHub (opcional)

Con acuerdo del usuario:

```bash
gh repo edit --add-topic rust,bitcoin --description "..."
```

Release (solo si hay tag/version listos):

```bash
git tag -a v0.1.0 -m "v0.1.0"
git push origin v0.1.0
gh release create v0.1.0 --title "v0.1.0" --notes "Notas de release"
```

Pull request (rama de feature, no publicación inicial):

```bash
git push -u origin HEAD
gh pr create --title "..." --body "$(cat <<'EOF'
## Summary
- ...

## Test plan
- [ ] ...
EOF
)"
```

---

## Reglas de seguridad (obligatorias)

- No modificar `git config` global del usuario.
- No commitear `.env`, seeds, claves privadas ni dumps de wallet.
- No `git push --force` a `main`/`master` sin petición explícita y advertencia.
- No push al remoto salvo petición explícita en la conversación o regla del usuario.

---

## Ejemplo de invocación

Usuario: *"Quiero publicar este proyecto en GitHub"*

1. Ejecutar pasos 1–2.
2. Mostrar checklist y hallazgos (secretos, .gitignore, README).
3. Preguntar: nombre del repo, público/privado, y si procede commit + push.
4. Continuar pasos 3–6 según respuestas.
