# HEx Gist Sync

CLI em Rust para deixar as configurações do HEx portáteis usando um gist do GitHub como "nuvem". Funciona no Linux e no Windows, então dá para usar a mesma configuração nos dois sistemas.

A CLI fala inglês. A documentação abaixo está em português.

**Sincroniza apenas** o arquivo `settings.json` do HEx, onde ficam atalhos, idioma, modelos de transcrição, modos de ditado e regras de roteamento. Nada de estado local é enviado: histórico de ditados, token da API local, variáveis de sessão e arquivos de token nunca saem da máquina.

## Como cada sistema encontra o settings.json

- Linux: `$XDG_DATA_HOME/hex/settings.json` ou `~/.local/share/hex/settings.json`.
- Windows: procura `hex/settings.json` e `anomaly/hex/settings.json` dentro de `%APPDATA%` e `%LOCALAPPDATA%`.

O primeiro arquivo existente é usado. Se você usar outro caminho, defina `HEX_SETTINGS_PATH` ou configure o seu binário com esse ambiente. O ID do gist fica em `~/.config/hex-gist-sync/gist-id` no Linux e em `%APPDATA%\hex-gist-sync\gist-id` no Windows, com override por `HEX_GIST_STATE_FILE`.

## O que não vai para o gist

Antes do upload, os valores abaixo são trocados por `__HEX_SYNC_LOCAL__`. No `pull`, eles são restaurados do `settings.json` da própria máquina:

- chaves e segredos, detectados por nome (`apiKey`, `token`, `secret`, `password`, `credential`, `auth`);
- `microphone`, que é o dispositivo de captura escolhido nesta máquina;
- pastas locais: `meetings.output_folder`, `capture.folder` e `screenshots.folder`;
- campos de integração: `capture.routing.jev`, `jevApiKey`, `agentHost` e `agentOpencode`;
- `migrations`, que é controle interno do app.

Se você adicionou uma chave nova localmente depois do último `push`, o `pull` também preserva esse valor.

## Requisitos

- GitHub CLI autenticado (`gh auth login`). Funciona com `GH_TOKEN` ou `GITHUB_TOKEN` no ambiente.
- Rust 1.70 ou superior para compilar. Não precisa de Python, bash, jq nem cmake em tempo de execução.

## Build e instalação

```bash
git clone <url-do-seu-repo> hex-gist-sync
cd hex-gist-sync
cargo build --release
```

O binário fica em `target/release/hex-gist-sync` (`.exe` no Windows). Para instalar no PATH:

```bash
cargo install --path .
```

No Windows, o mesmo comando funciona dentro do PowerShell ou do terminal Git Bash desde que o `cargo` esteja no PATH.

## Fluxo entre duas máquinas

Na primeira máquina:

```bash
hex-gist-sync init
```

Por padrão o gist é secreto, ou seja, não aparece na listagem pública, mas continua acessível por URL. Para criar público, use `hex-gist-sync init --public`. O comando imprime a URL do gist e salva o ID localmente.

Na segunda máquina:

```bash
hex-gist-sync use <id-ou-url-do-gist>
hex-gist-sync pull
```

Ou, sem salvar estado:

```bash
HEX_GIST_ID=<id-do-gist> hex-gist-sync pull
```

## Uso diário

```bash
hex-gist-sync push   # depois de mudar algo nas configurações do HEx
hex-gist-sync pull   # na outra máquina, para trazer as mudanças
hex-gist-sync diff   # compara o arquivo local com o gist, sem alterar nada
```

O `diff` lista os caminhos que divergiram. Se ele mostrar diferença, escolha `push` ou `pull` explicitamente: o script nunca decide a direção sozinho.

O `pull` salva um backup com timestamp ao lado do `settings.json` antes de substituir o arquivo. Feche o HEx antes de puxar e abra de novo depois, senão o app pode sobrescrever a alteração com o estado que está na memória. Isso vale tanto no Linux quanto no Windows.

## Variáveis de ambiente

| Variável | Finalidade |
| --- | --- |
| `HEX_GIST_ID` | ID do gist, tem precedência sobre o arquivo de estado |
| `HEX_SETTINGS_PATH` | Caminho do `settings.json` do HEx |
| `HEX_GIST_FILE` | Nome do arquivo dentro do gist, padrão `hex-settings.json` |
| `HEX_GIST_STATE_FILE` | Onde o ID do gist é salvo |
| `HEX_GIST_PUBLIC` | Use `1` no `init` para criar gist público |

## Segurança

O gist padrão é secreto. Mesmo assim, revise o conteúdo antes de usar `--public`: modos de pós-processamento podem conter prompts e regras próprias. Se alguma chave chegar a vazar, revogue e gere outra, não conte apenas com a exclusão do gist.
