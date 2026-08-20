# Backlog

Roteiro de `oc-voice` do estado atual até a versão pública: **controle de voz para Hyprland em português**, com ditado, envio de input e seleção de janela alvo.

Cada item tem um critério de aceite verificável por comando. Nada aqui depende de julgamento subjetivo para saber se está pronto.

Item concluído leva **✅ e o hash do commit** no título. Ao terminar um item, marque-o aqui no mesmo commit — é assim que o próximo agente sabe onde pegar.

## Onde estamos

O pipeline de áudio funciona: cpal → rubato 16 kHz → ring buffer → silero VAD → whisper `large-v3-turbo` Q8 em CUDA, com parciais a cada ~800 ms e final no silêncio. O overlay egui flutua e fixa via `hyprctl`. A injeção de texto via `wtype` funciona. Três modos existem: `Input`, `Translate`, `Enter`.

O que trava a evolução é o reconhecimento de comando: igualdade exata de string contra tabelas chumbadas. O caminho de LLM que existia — escrito e nunca ligado — foi removido em M0.1.

**Premissa central deste backlog:** o LLM sai. O conjunto de comandos é fechado e pequeno, e o conjunto de alvos é enumerável em runtime via `hyprctl clients -j`. Similaridade de string resolve os dois em microssegundos, com precisão maior que a de um modelo adivinhando contra uma tabela estática. A latência que fez o projeto parar era de um componente que o objetivo real não precisa.

---

## M0 — Limpeza e fundação

Remover o que não roda e quebrar o monólito, antes de construir por cima.

### M0.1 — Remover o caminho do LLM ✅ `f07e822`

O classificador por LLM em [`src/llm_classifier.rs`](src/llm_classifier.rs) nunca é instanciado. `main.rs` só chama `classify_with_fallback`, que é um método estático e não toca no servidor. Os três `#[allow(dead_code)]` (linhas 7, 29, 232) existem exatamente para calar o compilador sobre isso.

Apagar:

- `LlmClassifier::start` (linha 31) e `wait_for_ready`
- `LlmClassifier::classify` (linha 88)
- `extract_json` (linha 233)
- `impl Drop for LlmClassifier` e o campo `server`
- `const LLAMA_PORT` (linha 8)
- Os três atributos `#[allow(dead_code)]`
- Dependência `ureq` de [`Cargo.toml`](Cargo.toml)
- `llama-cpp` de [`flake.nix`](flake.nix)
- Receita `fetch-llm` e as variáveis `llm_name` / `llm_url` do [`justfile`](justfile)

`VoiceCommand` fica — é o vocabulário de comandos, não é do LLM. **`serde` e `serde_json` também ficam:** `VoiceCommand` deriva `Serialize`/`Deserialize` com cinco `#[serde(rename)]`, `focus_window_and_type` desserializa o `hyprctl clients -j`, e M1.3 traz `toml`, que depende de serde. Só `ureq` sai.

**Aceite:** `grep -rn "allow(dead_code)\|llama\|ureq" src/ Cargo.toml flake.nix justfile` retorna vazio. `just check` verde. `cargo build --release` sem warnings.

**Feito em `f07e822`.** Duas decisões além da letra do item: `llm_classifier.rs` virou `commands.rs` (módulo batizado de LLM sem LLM dentro é a confusão que este item remove, e é onde M0.2 o coloca), e `classify_with_fallback` virou `classify` (sem LLM não há de onde cair para trás). `ureq` permanece no `Cargo.lock` como build-dependency de `ort-sys`, via o crate do VAD silero — transitivo, fora do nosso controle.

### M0.2 — Quebrar `main.rs` em módulos ✅ `ee9c6d8`

1399 linhas com sete responsabilidades distintas. Um contribuidor externo não consegue achar nada, e é impossível testar as partes isoladamente.

Divisão proposta, seguindo as fronteiras que já existem no arquivo:

| Módulo | O que leva | Linhas de origem |
|---|---|---|
| `audio/capture.rs` | `run_capture`, `run_capture_system`, `get_default_sink_target` | 494–719 |
| `audio/resample.rs` | `Resampler16k`, `to_mono`, `push_samples` | 720–808 |
| `asr/mod.rs` | `transcribe`, `SpeechSegment`, `HALLUCINATIONS`, `filter_hallucination` | 422–471, 809–898 |
| `commands/mod.rs` | `execute_command` + o `src/commands.rs` atual | 335–381 |
| `wm/hyprland.rs` | `focus_window_and_type`, `try_hyprland_float`, `hyprctl_primary_monitor_info` | 1004–1191 |
| `input/inject.rs` | `type_text`, `type_key`, `type_shift_return` | 899–1003 |
| `ui/overlay.rs` | `run_overlay`, `OverlayApp`, `LANGUAGES` | 1075–1399 |

Linhas conferidas em `f07e822`. Se divergirem, confie nos nomes, não nos números.

`main.rs` fica com `main`, `run_audio_pipeline`, `TranscriptEvent`, `TranscribeMode`, `AppSettings`, `emit` e os `const` de tuning.

Junto vai um requisito que M3.1 depende: todo processo externo (`hyprctl`, `wtype`, `pw-record`) passa por um trait `CommandRunner` com implementação real e implementação de teste. Sem essa indireção nada que dispara comando é testável, e o critério de aceite de M3.1 é impossível de cumprir.

**Aceite:** `just limits` passa — é o gate que verifica o teto de 400 linhas por arquivo, e ele já está vermelho hoje por causa do `main.rs`. `just check` verde (roda `limits`, `check`, `clippy`, `fmt` e `test`). Zero mudança de comportamento — o binário roda igual.

### M0.4 — Gates de conformidade ✅ `1db6aad`

O teto de 400 linhas virou `just limits` porque critério escrito em prosa ninguém roda. Dois outros critérios deste backlog estão na mesma situação.

**`just vocab` — vocabulário fora do código.** Três itens exigem que nenhuma palavra falada nem nome de aplicativo apareça em `src/` (M1.3, M2.1, M3.1), e o `AGENTS.md` repete como diretriz. Hoje há **33 ocorrências**, medidas com `just vocab` (o item citava 28, de antes de M0.2 espalhar o código em módulos — as dicas do overlay e a fixture inline do `hyprctl` entraram na contagem):

```
29× src/commands/mod.rs   tabelas de keyword, prefixos send_to, aliases e testes
 2× src/ui/overlay.rs     dicas de UI ("say envia/cambio to send")
 2× src/wm/hyprland.rs    fixture inline do hyprctl com class "code"
```

O gate procura literais de string em `src/` contra duas listas — palavras de comando em português e nomes de aplicativo conhecidos — e reprova se achar. Nasce **vermelho** e só fica verde quando M1.3 e M2.1 moverem tudo para o `commands.toml`, igual ao `just limits`. Depois disso, impede que alguém acrescente "só um alias rapidinho" no código.

Exceção legítima: `commands.rs` pode conter o TOML default embutido via `include_str!`, que é configuração, não código. O gate ignora arquivos `.toml`.

**`just refs` — referência morta na documentação.** O `BACKLOG.md` cita 6 posições `arquivo:linha`. Todas envelheceram no M0.1: uma vez quando `llm_classifier.rs` virou `commands.rs`, outra quando o `main.rs` deslocou 4 linhas. Foram corrigidas à mão nas duas vezes. Envelheceram uma terceira vez no M0.2, quando `main.rs` virou módulos, e foram corrigidas no commit que ligou este gate.

É o modo de falha crônico deste repo — o `README` e o `AGENTS.md` ficaram mentindo por meses, e o backlog conseguiu ficar obsoleto dentro de um único commit. O gate extrai cada `arquivo:linha` dos documentos (fora de blocos de código), confere se o arquivo existe e se a linha ainda contém o símbolo que o texto afirma, e reprova na divergência. Toda referência precisa nomear o símbolo em backticks ao lado — é o que o gate verifica. Deve passar hoje.

**Aceite:** `just refs` verde e dentro do `just check`. `just vocab` vermelho hoje, listando as ocorrências, e **fora do `just check`** — ele entra lá em M2.1, quando de fato ficar verde.

**Correção pós-entrega, e o erro foi da especificação, não do código.** A versão original deste item mandava os dois gates entrarem no `just check` de imediato, e a implementação seguiu corretamente. Isso deixaria o build vermelho de propósito por seis itens seguidos (M0.5 até M2.1), e build cronicamente vermelho é pior que gate nenhum: ensina a ignorar o vermelho e esconde a primeira falha real. O `limits` pôde entrar de cara porque M0.2 vinha logo em seguida; o `vocab` não tem essa folga.

O gate também precisou passar a **ignorar blocos `#[cfg(test)]`**: os testes do matcher contêm por construção as palavras que eles casam, então o gate como escrito exigia apagar cobertura de teste para ficar verde — ele reprovava `assert_eq!(classify("envia"), ...)`. Corrigido em `6e493ef`; restam 25 ocorrências reais, 23 em `commands/mod.rs` e 2 nas dicas do overlay.

### M0.5 — Pânico silencioso na thread de áudio ✅ `08d2a2e`

O pipeline roda em thread separada e trata `Err`, mas não trata pânico:

```rust
let pipeline_handle = std::thread::spawn(move || {
    if let Err(e) = run_audio_pipeline(...) { error!(...); }
});
```

Há 8 `unwrap()` no `main.rs`, 3 deles no caminho de áudio. Se um estourar, a thread morre, o `if let Err` nunca executa, e o `join()` no fim descarta o pânico com `let _`. **O overlay continua de pé, com aparência normal, sem transcrever nada** — a pessoa fica falando com uma janela morta sem entender por quê.

Duas partes:

1. Detectar a thread morta — `is_finished()` no loop da UI, ou um canal que fecha — e mostrar no overlay que o pipeline caiu, em vez de aparentar normalidade.
2. Trocar os `unwrap()` do caminho de áudio por erro tratado. `settings.lock().unwrap()` em mutex envenenado é o caso mais provável.

**Aceite:** com um pânico injetado em `run_audio_pipeline`, o overlay mostra estado de falha em vez de silêncio. `clippy::unwrap_used` negado nos módulos do caminho de áudio.

**Feito.** A detecção usa o canal: o `Sender` mora na thread do pipeline, então pânico ou erro derrubam a thread, o canal desconecta e o overlay pinta o estado de falha — não precisa de `is_finished()`. Os `unwrap()` do caminho de áudio viraram `lock_settings`, que recupera mutex envenenado (`AppSettings` é dado puro). O `deny(clippy::unwrap_used)` foi aplicado nas declarações `mod asr;` / `mod audio;` e na `fn run_audio_pipeline` — `main.rs` é a raiz do crate, então um atributo inner ali negaria o lint no crate inteiro, incluindo overlay e o `FakeRunner` de teste, que não são caminho de áudio. O `join()` final agora loga o payload do pânico em vez de descartar com `let _`. A referência a `LANGUAGES` em M1.3 andou uma linha para baixo (campo novo no `OverlayApp`) e foi atualizada aqui.

### M0.3 — Renomear o binário ✅ `7859cf9`

`oc-voice-poc` não é mais um POC, e o nome vai aparecer para todo mundo que instalar. Trocar para `oc-voice` em [`Cargo.toml`](Cargo.toml) e nas receitas do [`justfile`](justfile).

O `EnvFilter` default em `main.rs` usa `oc_voice_poc=info` — precisa virar `oc_voice=info` junto, ou os logs somem silenciosamente.

**Aceite:** `just run` produz logs em nível `info`. `ls target/release/oc-voice` existe.

---

## M1 — Um matcher só, por similaridade

Hoje existem três lugares que comparam texto falado contra listas fixas, cada um com regra própria, e todos por igualdade exata: os comandos `classify` (`src/commands/mod.rs:48`), a tabela de aliases `resolve_target_alias` (removida em M2.1) e o filtro de alucinação `filter_hallucination` (`src/asr/mod.rs:97`). Igualdade exata é frágil contra ASR — foi o que causou o bug do "câmbio".

**Inventário: o que passa por similaridade, contra qual pool.** Cada linha é um pool **fechado e separado**; nenhum vê os candidatos do outro, e a etapa determina qual é consultado.

| Etapa | O que é comparado | Pool de candidatos | Operação |
|---|---|---|---|
| 1 | saída crua do ASR | lista de alucinações | `match_exact` |
| 2 | elocução inteira | `prefix` do idioma ativo | `match_exact` |
| 3 | resto da elocução | `send` / `cancel` / `newline` do idioma | `match_exact` |
| 3 | resto da elocução | `templates` do idioma | `match_template` |
| 4 | slot `{direcao}` | `directions` do idioma | `match_exact` |
| 4 | slot `{numero}` | `numbers` do idioma | `match_exact` |
| 4 | slot `{alvo}` | categorias → tokens de janelas vivas | dois estágios, M2.1 |
| 4 | slot `{monitor}` | posição, descrição, alias — M3.1 | token |
| 5 | elocução inteira | `confirm` do idioma — M4.3 | `match_exact` |

**Não passa por similaridade:** o texto ditado, que é injetado literal, e nomes de conector de monitor, que ninguém pronuncia.

### M1.1 — Matcher por distância de string ✅ `a7cbc6f`

Um módulo `commands/matcher.rs`: dado o texto falado e um conjunto de candidatos, devolver o melhor match acima de um limiar, ou nada.

A capacidade de **recusar** é o requisito central, não a de acertar. Um autocorretor de teclado é obrigado a escolher alguma coisa; se erra, custa uma palavra apagada. Aqui um erro digita dentro da janela errada ou fecha ela. Devolver `None` é o mecanismo de segurança.

Pipeline, nesta ordem:

1. **Normalizar** — minúsculas, `fold_diacritics` (já existe em `src/commands/mod.rs:102`), remoção de pontuação. Colapsar `qu`→`k` e `c`→`k` na mesma passada: é uma linha e cobre a confusão acústica mais comum do português.
2. **Filtrar por contagem de palavras** — só entram na comparação candidatos com o mesmo número de palavras da fala. Este passo é o que separa comando de ditado, ver medição abaixo.
3. **Pontuar** com Jaro-Winkler (`strsim`), limiar default 0.82.

**Por que o filtro de contagem de palavras existe.** Jaro-Winkler sozinho não serve: o bônus de prefixo do Winkler premia qualquer frase ditada que *comece* com palavra de comando. Medido contra a tabela real:

| ditado normal | casa com | score | |
|---|---|---|---|
| "limpa a tela toda" | limpa | 0.86 | falso positivo |
| "pronto falei" | pronto | 0.90 | falso positivo |
| "manda ver o resultado disso" | manda | 0.84 | falso positivo |

Penalidade por diferença de comprimento de caractere resolve esses três mas derruba "enviar", "mandar" e "quambio" junto — é grosseira demais. O filtro por contagem de palavras separa limpo, porque nenhum comando tem 4 palavras.

Resultado do pipeline completo, 17 casos:

| entrada | esperado | resultado |
|---|---|---|
| `cambio`, `sambio`, `cambiu`, `kambio`, `quambio`, `cambrio` | Send | 0.85–1.00, todos reconhecidos |
| `enviar`, `mandar`, `cancelar` | Send / Cancel | 0.97 |
| `nova linha`, `nova linia` | Newline | 0.96–1.00 |
| `limpa a tela toda`, `manda ver o resultado disso`, `envia isso pro cliente amanha` | ditado | recusados antes de pontuar |
| `pronto falei`, `sao paulo`, `bom dia` | ditado | 0.55–0.59, abaixo do limiar |

**Zero erros em 17.** Camada fonética completa (metaphone e similares) foi medida e só altera `kambio` e `quambio`, que já passavam — não justifica um estágio próprio, por isso virou uma linha na normalização.

**Duas operações, não uma.** O acima resolve comando **sem argumento** — a elocução inteira é um comando. Mas todo comando de M3.1 carrega argumento, e aí o filtro de contagem de palavras recusa tudo:

| falado | palavras | candidatos de mesmo tamanho |
|---|---|---|
| "monitor da direita" | 3 | nenhum → recusado |
| "área de trabalho quatro" | 4 | nenhum → recusado |
| "leva pra três" | 3 | nenhum → recusado |

Então o módulo expõe duas funções:

- `match_exact(fala, candidatos)` — elocução inteira, com gate de contagem de palavras. Serve `send`, `cancel`, `newline`, confirmações.
- `match_template(fala, templates)` — casa contra padrões com slot: `"monitor da {direcao}"`, `"area de trabalho {numero}"`, `"foca o {alvo}"`. O gate de contagem de palavras se aplica ao **template preenchido**, não à fala crua, e cada slot é resolvido pela tabela do seu tipo (`directions`, `numbers`) ou pelo resolvedor de alvo de M2.1.

Os templates moram no `commands.toml` junto do resto do vocabulário, porque a ordem das palavras muda por idioma — "monitor da direita" contra "right monitor".

**Aceite:** teste cobrindo cada uma das 17 entradas acima em `match_exact`, mais as 3 frases da tabela de templates resolvendo em `match_template`. `cargo test` verde.

### M1.2 — Trocar as três comparações pelo matcher ✅ `57cacc2`

Reescrever `classify` (`src/commands/mod.rs:48`) usando o matcher, remover a guarda `words.len() > 5`, e passar o filtro de alucinação pelo mesmo caminho.

Um bug irmão do "câmbio" que some junto: hoje o match é igualdade contra a **string inteira** normalizada. Existe uma guarda de ≤5 palavras sugerindo que frases curtas deveriam passar, mas na prática só a palavra sozinha funciona — "ok câmbio" cai como ditado.

**Aceite:** `grep -rn "eq_ignore_ascii_case\|contains(&text_lower)" src/` vazio. Os testes de M1.1 passam contra a API pública.

### M1.3 — Vocabulário multilíngue em arquivo de configuração ✅ `4bec2b2`

As palavras estão no código-fonte, em português, com o alvo `oc-opencode` chumbado. O overlay já deixa escolher entre 8 idiomas de transcrição (`LANGUAGES`, `src/ui/overlay.rs:56`), mas os comandos só existem em português — trocar o idioma faz o ditado funcionar e os comandos pararem.

Mover para `~/.config/oc-voice/commands.toml`, com seções por idioma e `pt` + `en` embutidos no binário como default:

```toml
[matching]
threshold = 0.82

[pt]
prefix  = ["computador"]
send    = ["câmbio", "envia", "manda", "pronto"]
cancel  = ["cancela", "limpa", "descarta"]
newline = ["nova linha", "pula linha"]
numbers = { um = 1, dois = 2, três = 3, quatro = 4 }
directions = { direita = "r", esquerda = "l", cima = "u", baixo = "d" }

[en]
prefix  = ["computer"]
send    = ["send", "go", "over"]
cancel  = ["cancel", "clear", "discard"]
newline = ["new line"]
numbers = { one = 1, two = 2, three = 3, four = 4 }
directions = { right = "r", left = "l", up = "u", down = "d" }
```

As tabelas de números (M3.2) e direções (M3.1) moram aqui também — são vocabulário falado como qualquer outro, e seria incoerente traduzir "envia" mas não "direita".

**Seleção do idioma ativo.** Segue `AppSettings.language`. Quando está em `auto`, `transcribe` passa `set_language(None)` e o whisper detecta sozinho — recuperar o resultado com `full_lang_id()` e usar a tabela correspondente, com fallback para a última tabela conhecida se o idioma detectado não tiver seção no arquivo.

**Limite conhecido, e é preciso ser explícito no README.** O matcher de M1.1 depende de duas premissas que só valem para escrita alfabética com espaço entre palavras: a dobra de diacríticos e o filtro por contagem de palavras. Japonês e chinês não têm espaço entre palavras — o filtro colapsa e o Jaro-Winkler sobre ideogramas não mede a mesma coisa. `ja` e `zh` continuam funcionando para **transcrição e ditado**, mas não recebem comandos de voz. Suporte a comando para CJK exige tokenizador próprio e fica fora deste roteiro.

As duas dicas de texto do overlay (`src/ui/overlay.rs`) citam "envia" e "cambio" chumbados — também passam a vir da config do idioma ativo, senão a UI anuncia comando em português para quem selecionou inglês.

**Aceite:** rodar sem config nenhuma funciona igual a hoje, em português. Trocar o idioma do overlay para `en` faz os comandos em inglês funcionarem sem recompilar. Um `commands.toml` com uma seção `[es]` escrita pelo usuário passa a funcionar ao selecionar espanhol. Selecionar `ja` transcreve normalmente e não dispara comando nenhum.

---

## M2 — Alvos dinâmicos

### M2.1 — Matar a tabela de aliases ✅ `1085b2a`

`resolve_target_alias` (removida por este item) traduz a palavra falada para um nome de classe chumbado, e só então `focus_window_and_type` procura essa classe nas janelas vivas. A tradução corrompe a busca.

Medido nas janelas abertas nesta máquina, **5 dos 7 aliases não encontram nada**:

| falado | tabela chuta | resultado real |
|---|---|---|
| navegador | `firefox` | não acha — o navegador aqui é `brave-browser` |
| chrome | `chromium` | não acha |
| opencode | `oc-opencode` | não acha |
| chat | `discord` | não acha — o chat aqui é Teams em `electron` |
| telegram | `telegram` | não acha |
| terminal | `Alacritty` | acha |
| editor | `code` | acha |

**O erro da tabela é o mapeamento 1-para-1, não a existência dela.** Jogar a palavra falada direto no matcher de M1.1 contra `class` e `title` também não funciona — medido nas janelas vivas desta máquina:

| falado | melhor candidato | score | |
|---|---|---|---|
| navegador | `brave-browser` | 0.58 | recusado |
| terminal | título do Teams | 0.64 | recusado |
| editor | título do VS Code | 0.68 | recusado |
| teams | título do Teams | 0.55 | recusado |
| brave | `brave-browser` | 0.88 | ok |

Só o nome literal passa. Duas causas independentes:

1. **Alias semântico não é semelhança de grafia.** "navegador" e `brave-browser` não se parecem — a relação é de significado. Nenhuma métrica de string resolve isso.
2. **Título longo destrói o score.** "teams" está literalmente dentro de `Chat | Team 8 - dev internal | Microsoft Teams` e pontua 0.55, porque Jaro-Winkler mede a string **inteira** e pune a diferença de comprimento.

Resolução em dois estágios:

**Estágio 1 — categoria.** Seção `[targets]` por idioma no `commands.toml`, mapeando categoria falada para uma **lista** de padrões de classe, não para um nome único:

```toml
[pt.targets]
navegador = ["firefox", "brave", "chromium", "chrome", "zen", "vivaldi"]
terminal  = ["alacritty", "kitty", "foot", "wezterm", "ghostty"]
editor    = ["code", "nvim", "zed", "emacs"]
chat      = ["discord", "telegram", "slack", "teams", "element"]
```

A fala casa contra o **nome da categoria**; os padrões da categoria são então testados contra as janelas vivas. Quem estiver aberto vence — não importa qual navegador a pessoa usa.

**Estágio 2 — token direto.** Se nada casou como categoria, comparar contra cada **token** de `class` e `title` separadamente, tomando o melhor. É isso que corrige o problema do título longo: `Microsoft Teams` tokeniza em `microsoft` e `teams`, e "teams" bate 1.00.

Medido com os dois estágios, mesmas janelas:

| falado | resolve para | via |
|---|---|---|
| navegador | `brave-browser` | categoria |
| terminal | `Alacritty` | categoria |
| editor | `code` | categoria |
| chat | `electron` (Teams) | categoria |
| teams, brave, remmina | a janela certa | token |
| fotoshop (não está aberto) | recusado, 0.71 | — |

**Os dois pools nunca se misturam.** Vocabulário de comandos e janelas vivas são conjuntos de candidatos **separados**, escolhidos pela gramática de M1.1 antes de qualquer pontuação: `match_exact` só vê comandos, um slot `{alvo}` só vê janelas. Um pool único faria "câmbio" (enviar) competir com uma aba de navegador chamada "Câmbio do dólar".

A lista oposta serve como **sinal de ambiguidade**, não como candidata: resolvido o alvo, pontuar o mesmo texto contra o vocabulário de comandos; pontuação alta nos dois significa conflito, e cai na política de confirmação de M4.3.

**Duas regras de robustez, medidas contra as janelas reais.** O pool de alvos é volátil — títulos mudam enquanto se trabalha e carregam texto arbitrário, inclusive o que a própria pessoa acabou de ditar:

| regra | motivo | efeito medido |
|---|---|---|
| descartar tokens com menos de 3 caracteres | tokens de uma letra (`e`, `o`) pontuam 0.72–0.76 contra qualquer coisa, puro ruído | remove o ruído sem custo |
| limiar de `title` em 0.90, `class` em 0.82 | `class` é estável (`code`, `brave-browser`); `title` é texto arbitrário | pior colisão ("direita" contra um título contendo "divida", 0.80) sai de 3% para 11% de folga |

O limiar mais duro em título é de graça: alvo legítimo casa por token exato e pontua 1.00 — `teams`, `brave`, `remmina` e `alacritty` medidos, todos 1.00.

**Aceite:** `just vocab` verde, e passa a integrar o `just check` a partir deste item. `resolve_target_alias` não existe mais. As 7 linhas da tabela de resolução viram teste, com um JSON de `hyprctl clients` fixo em `tests/fixtures/`. Uma fixture com janela titulada "Câmbio do dólar" não intercepta o comando `send`. Nenhum nome de aplicativo aparece no código-fonte — todos vêm do `commands.toml`.

### M2.2 — Corrigir o match vazio em `focus_window_and_type` ✅ `1085b2a`

Na antiga `focus_window_and_type` a condição `target_lower.contains(&class)` era verdadeira sempre que `class` é string vazia, porque `contains("")` é sempre `true`. Uma janela sem classe captura qualquer alvo falado.

**Aceite:** candidatos com `class` e `title` vazios são descartados antes de comparar. Teste com fixture contendo uma janela de classe vazia.

### M2.3 — Feedback de alvo no overlay ✅ `976b8c8`

Focar a janela errada e digitar dentro dela é destrutivo e não tem desfazer. O overlay precisa mostrar o alvo resolvido e o score de confiança antes de injetar.

Este item cobre só a **exibição**. Quando pedir confirmação é decidido pela política única de M4.3 — não invente um segundo mecanismo aqui.

**Aceite:** `TranscriptEvent::SentTo` carrega o score. O overlay renderiza alvo e confiança.

---

## M3 — Navegação Hyprland

O objetivo do projeto. É a parte mais fácil: gramática fechada, estado enumerável via `hyprctl`, feedback visual instantâneo, ações reversíveis.

### M3.1 — Módulo de dispatch ✅ `eb458c4`

`wm/dispatch.rs`, envolvendo `hyprctl dispatch`:

| Falado | Dispatch |
|---|---|
| "monitor da direita" / "monitor esquerdo" | `focusmonitor r` \| `l` |
| "monitor do meio" / "monitor Samsung" | `focusmonitor <nome>` — ver resolução abaixo |
| "janela de cima / baixo / esquerda / direita" | `movefocus u` \| `d` \| `l` \| `r` |
| "área de trabalho <n>" | `workspace <n>` |
| "leva pra <n>" | `movetoworkspace <n>` |
| "foca o <alvo>" | `focuswindow address:<addr>` via matcher de M2.1 |
| "tela cheia" | `fullscreen` |
| "flutuante" | `togglefloating` |
| "fecha" | `killactive` |

Todos os alvos e direções passam pelo matcher de M1.1, e as palavras de direção vêm da tabela `directions` do idioma ativo (M1.3).

**Como nomear um monitor por voz.** Monitores vêm de `hyprctl monitors -j`, mas o **nome do conector não é falável** — ninguém diz "HDMI-A-1" nem "eDP-1". Esses nomes ficam fora do pool de similaridade; são só o valor final passado ao dispatch. Três formas de nomear, nesta ordem de precedência:

| Forma | Origem | Exemplo nesta máquina |
|---|---|---|
| posição | ordenar por `x` do `hyprctl monitors -j` | `eDP-1` (x=0) é "o da esquerda", `HDMI-A-1` (x=1536) "o do meio", `DP-1` (x=4976) "o da direita" |
| marca ou modelo | token contra `description` | "monitor Samsung" → `DP-1`, "monitor LG" → `HDMI-A-1` |
| alias do usuário | `[pt.monitors]` no `commands.toml` | "monitor principal" → o que a pessoa definir |

A posição é derivada, não configurada: mudou o arranjo físico, "o da direita" acompanha sem editar nada.

**Aceite:** cada linha da tabela de dispatch tem um teste que verifica o `hyprctl` montado, com o `CommandRunner` de teste de M0.2. Nenhum nome de conector, marca ou modelo aparece no código-fonte. Fixture com três monitores em posições trocadas resolve "o da direita" para o de maior `x`.

### M3.2 — Números por extenso ✅ `eb458c4`

Whisper em português emite "quatro", não "4". Sem isso, nenhum comando de workspace funciona.

Mapa palavra→dígito de 1 a 10, na tabela `numbers` da seção de idioma de M1.3. Cada idioma traz a sua — "quatro", "four", "cuatro".

**Aceite:** "área de trabalho quatro" e "área de trabalho 4" produzem o mesmo dispatch. Testes para 1–10 em `pt` e `en`.

### M3.3 — Classificar `killactive` como destrutivo ✅ `eb458c4`

Fechar janela é a única ação da lista que destrói trabalho e não tem desfazer. Um falso positivo do ASR custa caro.

Não é um mecanismo próprio: é marcar o comando como destrutivo para a política de M4.3 pegar. `fullscreen` e `togglefloating` são reversíveis e não entram.

**Aceite:** `killactive` marcado como destrutivo. "fecha" sozinho não fecha nada; "fecha" → "confirma" fecha.

---

## M4 — Comando ou ditado

O problema de projeto que realmente sobra. Não é o `hyprctl` — é decidir se "nova linha" é um comando ou parte da frase que a pessoa está ditando.

### M4.1 — Palavra-prefixo ✅ `68541e7`

A heurística atual (≤5 palavras) é frágil nas duas pontas: bloqueia comandos legítimos mais longos e dispara em ditado curto.

Trocar por um prefixo explícito, vindo da chave `prefix` do idioma ativo (M1.3) — "computador" em pt, "computer" em en:

- "computador, monitor da direita" → comando
- "monitor da direita" → ditado literal

O prefixo não substitui o filtro de contagem de palavras de M1.1 — ele age antes. Reconhecido o prefixo, o **resto** da elocução é que vai para `match_exact` ou `match_template`, e o filtro se aplica a esse resto. Os dois se compõem: o prefixo elimina o falso positivo em ditado, o filtro elimina o falso positivo dentro de fala já marcada como comando.

**Aceite:** com o prefixo ativo, ditar qualquer palavra de comando sem o prefixo produz texto literal. Testes cobrindo as duas direções.

**Decisão de implementação:** o prefixo é controlado por `require_prefix` (default `false`) na seção do idioma. Ligado por config, não por presença de palavras na lista — o default com prefixo obrigatório quebraria o aceite de M1.3 ("rodar sem config funciona igual a hoje", onde "câmbio" sozinho envia). Com `require_prefix = false`, o prefixo é aceito mas não exigido.

### M4.2 — Modo `Command` ✅ `57b3f9e`

`TranscribeMode` já tem `Input`, `Translate` e `Enter`. Adicionar `Command`, onde tudo é interpretado como comando de WM e nada é ditado — sem precisar do prefixo.

**Aceite:** o seletor de modo do overlay lista os quatro. Em `Command`, `type_text` nunca é chamado.

### M4.3 — Política única de confirmação ✅ `976b8c8`

Três itens deste backlog pediam confirmação por caminhos diferentes: alvo de baixa confiança (M2.3), ação destrutiva (M3.3) e comando reconhecido errado. Três mecanismos separados viram três comportamentos inconsistentes. Um só, com duas entradas:

| Gatilho | Comportamento |
|---|---|
| Comando marcado destrutivo (M3.3) | sempre pede confirmação explícita |
| Score do match abaixo de `confirm_below` (default 0.9) | mostra no overlay e pede confirmação |
| Nenhum dos dois | executa direto, sem atraso |

**Não há janela de atraso.** A versão anterior deste item propunha ~400 ms entre reconhecer e disparar, cancelável por voz. Isso custa 400 ms em **todo** comando, inclusive nos de alta confiança, num projeto que parou por causa de latência. A confirmação é sob demanda, e o caso comum não paga nada.

`confirm_below` e a lista de destrutivos ficam no `commands.toml`, e podem ser zeradas por quem preferir agir sempre direto.

**Aceite:** comando de alta confiança e não destrutivo dispara sem atraso mensurável. "computador, fecha" espera; "confirma" executa; "não" descarta. Um `confirm_below = 0.0` com lista de destrutivos vazia faz tudo disparar direto.

---

## M5 — Pronto para publicar

### M5.1 — Licença e metadados ✅

Não há `LICENSE`, e [`Cargo.toml`](Cargo.toml) não tem `license`, `repository`, `authors` nem `readme`. Sem isso o projeto não é legalmente utilizável por ninguém.

**Aceite:** `LICENSE` existe e o mesmo identificador SPDX está em `Cargo.toml`. `cargo metadata` mostra `license`, `repository`, `authors` e `readme` preenchidos. (Não usar `cargo publish` como critério — é um binário atrelado a CUDA, não vai para o crates.io.)

**Desvio consciente:** `repository` ficou de fora — o repo ainda não tem remote, e inventar a URL seria pior que omitir. Preencher no momento do push ao GitHub; há um comentário no `Cargo.toml` marcando o lugar.

### M5.2 — Reescrever README e AGENTS ✅

Os dois estão defasados em pontos que vão enganar qualquer pessoa nova. O [`README.md`](README.md) descreve overlay, VAD, Hyprland e resampling decente como **fora de escopo** — está tudo implementado — e cita `ggml-base.en.bin`, modelo que o [`justfile`](justfile) não usa desde que virou `large-v3-turbo`. O [`AGENTS.md`](AGENTS.md) repete as mesmas exclusões.

O `shellHook` do [`flake.nix`](flake.nix) imprime a mesma mensagem obsoleta toda vez que alguém entra no dev shell — é o primeiro texto que um contribuidor lê.

README novo precisa cobrir: o que é, requisitos reais (NixOS + flakes + driver NVIDIA + Hyprland + Wayland), instalação, tabela de comandos de voz, formato do `commands.toml` com exemplo de idioma novo, e o que **não** funciona (só Hyprland; só GPU NVIDIA testada; comandos só em idiomas com espaço entre palavras).

**Aceite:** nenhum dos três arquivos cita `base.en`. Nenhum lista como fora de escopo algo que existe no código.

### M5.3 — CI

`just check` roda só na sua máquina. Sem CI, o primeiro PR externo quebra o build sem ninguém perceber.

GitHub Actions com `nix develop --command just check` mais `cargo test`. Usar a feature `cpu` — runner não tem GPU.

**Aceite:** o workflow passa no CI. Um PR com `cargo fmt` sujo é reprovado.

### M5.4 — Caminho sem CUDA ✅

`default = ["cuda"]` em [`Cargo.toml`](Cargo.toml). Quem não tem NVIDIA precisa descobrir sozinho a flag certa, e o [`README.md`](README.md) não menciona.

Documentar `just run-cpu` e medir a latência real de `large-v3-turbo` em CPU — se for inviável, recomendar um modelo menor explicitamente, com número medido.

**Aceite:** README traz latência medida nas duas trilhas, com o hardware nomeado.

---

## Ordem de execução

M0 primeiro — mexer em código morto depois de construir por cima dele custa o dobro. Os gates de M0.4 entram cedo mesmo nascendo vermelhos: o `just vocab` vermelho é o que garante que M1.3 e M2.1 não sejam dados por prontos pela metade. Depois M1, que é o alicerce de tudo que vem: M2, M3 e M4 dependem todos do matcher. M2 antes de M3 porque a resolução de alvo é reusada pelo `focuswindow`. M4 pode ir em paralelo com M3, com uma exceção: M4.3 é a política de confirmação que M2.3 e M3.3 consomem, então precisa existir antes de qualquer um dos dois ser fechado. M5 fecha.

O caminho mais curto até algo demonstrável é **M0.1 → M1.1 → M1.2 → M2.1**: remove o LLM, coloca similaridade no lugar e faz o alvo de janela funcionar de verdade. Isso já destrava o modo `Enter` que existe hoje e conserta os 5 aliases quebrados.

## Fora de escopo

Registrado para não voltar como dúvida:

- **Classificação por LLM.** Removida em M0.1. Se um dia o vocabulário virar aberto de verdade, o caminho é decodificação restrita por gramática GBNF com thinking desligado, não o prompt de texto livre que estava aqui.
- **Wake word acústica.** O prefixo de M4.1 é textual e resolve o mesmo problema sem outro modelo na GPU.
- **TTS e fallback por API remota.**
- **Compositores além do Hyprland.** `wtype` e `hyprctl` são premissas assumidas.
- **Comandos de voz em japonês e chinês.** Transcrição e ditado seguem funcionando; comando exige tokenizador de escrita sem espaço, ver M1.3.
