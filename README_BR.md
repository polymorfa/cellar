# cellar

[![Read in English](https://img.shields.io/badge/Read%20in-English-0b7285?style=for-the-badge)](README.md)

> This document is also available in [English](README.md).

Um arquivo pesquisável do código-fonte que a Meta entrega aos clientes. Baixe uma
versão do WhatsApp Web, compare com outra, leia qualquer módulo e descubra o que
depende do quê.

O WhatsApp Web é entregue como dezenas de milhares de arquivos JavaScript
minificados. Cada arquivo empacota várias definições `__d("NomeDoModulo", [deps],
factory, id)` em uma única linha. O `cellar` baixa uma versão do cliente, analisa
com a AST do [oxc](https://oxc.rs) e grava um arquivo por módulo, nomeado conforme o
módulo. O resultado é um diretório em que `grep -r`, seu editor e seu agente de IA
funcionam diretamente.

## Recursos

- Baixe e gerencie bundles de qualquer versão anterior do cliente.
- Compare duas versões usando um filtro nomeado, em JSON, NDJSON, Markdown ou texto.
- Busque módulos por nome, pelo código interno ou pelo que exportam.
- Veja o código de qualquer módulo, com dependências, dependentes e exports.
- Monte grafos de dependências e dependentes, em JSON, DOT ou Mermaid.
- Disponibilize tudo isso para um agente de IA via MCP.
- Suporta `whatsapp`, `messenger`, `facebook` e `instagram`.

## Requisitos

- Uma toolchain estável recente do Rust (1.95 ou superior).
- O [`just`](https://github.com/casey/just), que roda os atalhos de instalação e CI.
- Cerca de 1,3 GB de disco por versão indexada.

## Instalação

Instale o `just` primeiro:

```bash
brew install just        # macOS
cargo install just       # em qualquer lugar que tenha Rust
```

Outras opções, incluindo Debian, Fedora, Arch, Nix, Scoop e um binário pronto, estão
no [guia de instalação do `just`](https://github.com/casey/just#installation).

Depois compile o cellar:

```bash
git clone https://github.com/polymorfa/cellar
cd cellar
just install
```

Isso coloca o `cellar` em `~/.cargo/bin/cellar`. Confira se `~/.cargo/bin` está no seu
`PATH`.

## Documentação

A documentação completa, incluindo um exemplo real que revela um recurso não anunciado
em quatro comandos, está em **[cellar.mintlify.site](https://cellar.mintlify.site/pt-BR)**.

| Página | O que cobre |
| --- | --- |
| [Assistentes de IA](https://cellar.mintlify.site/pt-BR/agents) | Configuração MCP para Claude Code e Codex, e as dezesseis ferramentas |
| [Um exemplo real](https://cellar.mintlify.site/pt-BR/walkthrough) | Achando vínculo por passkey antes do lançamento |
| [Gerenciando versões](https://cellar.mintlify.site/pt-BR/bundles) | Baixar, importar, inspecionar e remover releases |
| [Encontrando código](https://cellar.mintlify.site/pt-BR/search) | Busca por nome, código ou símbolo exportado |
| [Comparando releases](https://cellar.mintlify.site/pt-BR/diff) | Diffs em texto, JSON, NDJSON ou Markdown |
| [Grafos](https://cellar.mintlify.site/pt-BR/graph) | Grafos de dependência em Mermaid, Graphviz ou JSON |
| [Filtros](https://cellar.mintlify.site/pt-BR/filters) | Reduzindo 187.000 módulos ao que importa |

Todo comando também aponta para a própria página no `--help`.

## Estrutura do projeto

| Crate | Papel |
| --- | --- |
| `cellar-core` | Layout de armazenamento, modelo do índice, filtros, diff e grafos. Sem I/O além do sistema de arquivos. |
| `cellar-index` | Análise do bundle com oxc até virar um índice de módulos. |
| `cellar-fetch` | Descoberta de versão, download e extração. A única crate que acessa a rede. |
| `cellar` | CLI e servidor MCP, ambos sobre uma única camada de operações. |

## Desenvolvimento

```bash
just ci        # fmt-check, clippy, testes, verificação da skill
```

Nenhum teste pode acessar a rede. O CI garante isso.

## Créditos

- O [wa-spec](https://github.com/vinikjkkj/wa-spec), do vini, que extrai diariamente
  os bindings de protocolo do WhatsApp Web dos mesmos bundles.
- O [whatspec](https://github.com/oxidezap/whatspec), de João Lucas, que extrai uma IR
  de protocolo tipada dos mesmos bundles e influenciou o design daqui.
- O `wa-diff-analyzer` do ProtoCocktail, minha tentativa anterior neste mesmo
  problema, cujo conjunto de filtros de módulo é portado aqui como o filtro `default`.
- O [WARDEN](https://warden-re.io), meu próprio projeto de engenharia reversa, que
  inspirou este aqui.
- O [meta-code-verify](https://github.com/facebookincubator/meta-code-verify), a
  extensão de integridade de código da própria Meta, que documenta o endpoint
  `btarchive`.

## Suporte

Se você quer suporte de nível empresarial do Rajeh, é possível agendar uma videochamada.
Reserve um horário de 1 hora entrando em contato com ele no Discord ou fazendo o
pré-agendamento [aqui](https://purpshell.dev/book). Quanto antes você reservar, melhor,
porque os horários costumam encher rápido.

Se você representa uma empresa, incentivamos que contribua de volta com os custos de
desenvolvimento do projeto. Você pode fazer isso agendando reuniões ou patrocinando
abaixo. Todo apoio é bem-vindo, de empresas de qualquer tamanho.

## Patrocínio

Se você quiser apoiar financeiramente este projeto, pode fazer isso
[aqui](https://purpshell.dev/sponsor).

## Aviso legal

> [!CAUTION]
> Este projeto não é afiliado, associado, autorizado, endossado ou de qualquer forma
> oficialmente ligado ao WhatsApp ou a qualquer uma de suas subsidiárias ou afiliadas.
> O site oficial do WhatsApp é whatsapp.com. "WhatsApp", bem como nomes, marcas,
> emblemas e imagens relacionados, são marcas registradas de seus respectivos donos.
>
> O `cellar` lê bundles do cliente servidos publicamente para pesquisa de
> interoperabilidade. Os mantenedores não apoiam o uso deste projeto em práticas que
> violem os Termos de Serviço do WhatsApp, e apelam à responsabilidade pessoal de seus
> usuários para usá-lo de forma justa.

## Licença

Copyright (c) 2026 Rajeh Taher
Copyright (c) 2026 Polymorfa (Softium LLC)

Licenciado sob a Licença MIT. Veja o [LICENSE](LICENSE) para o texto completo.
