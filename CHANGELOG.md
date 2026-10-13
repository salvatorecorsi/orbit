# Changelog

## 2026-10-13 — Palette flottante di creazione

### Richiesta
Aprire con Ctrl+T una palette flottante compatta, nello stile di VS Code, per creare finestre e progetti.

### Modifiche effettuate
- `services/shell.html`, `services/shell.js`, `services/shell.css`: dialogo di creazione separato dai pannelli laterali, centrato sopra la scena, con modalità Finestra e Progetto, focus confinato e chiusura tramite Escape, croce o clic esterno; conservati gli input quando Ctrl+T viene ripetuto.
- `src/webviews.rs`, `src/navigation.rs`: geometria della palette trasmessa via IPC e regione nativa limitata alla barra e al dialogo, mantenendo visibili scena e pagine sottostanti; sospesi gli input sullo sfondo durante la creazione e conservato il focus della palette quando diventano pronti pagine e popup.
- `README.md`: documentati apertura, chiusura e comportamento della palette.

### Note
- Riutilizzati i comandi di creazione esistenti, senza nuove dipendenze.
- Sintassi JavaScript, formattazione Rust, `cargo check --offline` e otto test Rust superati. Eseguibile compilato e verificato nelle copie isolate `.runtime/floating-qa` e `.runtime/floating-qa-final`: creazione di progetto e browser, scena e pagina visibili dietro la palette, Ctrl+T dal focus della pagina, bozza conservata, clic esterno senza attivare il contenuto sottostante ed Escape senza ridurre la pagina ingrandita.

---

## 2026-10-13 — Creazione di finestre e progetti dalla barra

### Richiesta
Aggiungere un tasto per creare una nuova finestra o un nuovo progetto e rendere disponibile la scorciatoia Ctrl+T.

### Modifiche effettuate
- `services/shell.html`, `services/shell.js`, `services/shell.css`: comando + Nuovo con scorciatoia visibile, pannello con modalità Finestra e Progetto, creazione del primo progetto nel workspace vuoto, focus senza spostamento della pagina e chiusura con Escape; controlli accessibili e layout compatto, riutilizzando l'IPC esistente.
- `src/webviews.rs`: filtro dei modificatori per distinguere Ctrl+T dalle combinazioni con Alt o Shift nelle WebView.
- `src/navigation.rs`, `src/main.rs`: gestione ordinata degli eventi tastiera nativi per riconoscere Ctrl+T anche quando i modificatori vengono premuti e rilasciati rapidamente.
- `services/project-store.js`, `services/project-store.test.js`: recupero del workspace quando mancano intere cartelle dei progetti, con copia integrale di `workspace.info` in `workspace.recovery-UUID.info` prima del salvataggio e pulizia dei riferimenti a scene, finestre e collegamenti mancanti; mantenuto l'errore per cartelle presenti ma invalide e aggiunti test di regressione.
- `services/host.test.js`: fixture sintetica per rendere indipendenti i test dai progetti locali.
- `package.json`: inclusi i test del recupero nello script `test:services`.
- `README.md`: documentati il comando di creazione, la scorciatoia e il recupero del workspace.

### Note
- Il recupero corregge il blocco all'avvio incontrato con il solo `workspace.info` presente nella cartella dei progetti.
- Build Rust e formattazione superati; quattro test di Alt+Tab e quattro test di Ctrl+T superati, inclusi pressione e rilascio nello stesso frame, modificatori aggiuntivi, ripetizioni e perdita del focus. Undici test dei servizi superati, inclusi otto test del recupero.
- Verifica UI nelle copie isolate `.runtime/creation-qa` e `.runtime/creation-keyboard-qa`: creati un progetto e una finestra browser locale, verificati Ctrl+T dalla scena vuota, dalla barra e dalla pagina ingrandita e chiusura del pannello con Escape. Registro originale dei progetti invariato, verificato con SHA-256.

---

## 2026-10-11 — Intercettazione nativa di Alt+Tab corretta

### Richiesta
Correggere Alt+Tab dentro ORBIT: il comando usciva dall'app invece di passare alla schermata successiva del pianeta corrente.

### Modifiche effettuate
- `src/shortcuts.rs`: rimossa la registrazione Raw Input della tastiera di Winit prima dell'installazione dell'hook, mantenendo gli eventi ordinari della tastiera e il Raw Input del mouse.
- `src/shortcuts.rs`: installazione e rimozione dell'hook al cambio di primo piano e di ambito, stato di disponibilità aggiornato e rilascio di Tab già intercettato prima della disattivazione.
- `README.md`: causa del problema e distinzione tra ciclo simulato, input nativo iniettato e prova con tastiera fisica.

### Note
- Prova isolata con `SendInput`: sei eventi ricevuti dall'hook, un cambio finestra eseguito e ORBIT rimasto in primo piano, senza ripristinare forzatamente il focus. La prova manuale con tastiera fisica resta distinta.
- Prova completa nativa `--smoke`, senza simulazione: 48 controlli superati in 47,32 secondi, nessun errore; report `.runtime/native-cycle/native-cycle-report.json`. Verificati Alt+Tab e Alt+Shift+Tab attraverso l'hook, focus conservato, cambio rapido di destinazione, annullamento, input Unicode, PowerShell nella finestra inclinata, mouse trusted, popup, editor e chat. L'input della prova è inviato tramite `SendInput`.
- Build senza warning, quattro test `shortcuts::tests` e `cargo fmt --check` superati.

---

## 2026-10-11 — Cambio finestra con Alt+Tab e camera orbitale

### Richiesta
Passare tra le schermate aperte del pianeta corrente con Alt+Tab, spostando l'inquadratura verso la finestra scelta. Alt+Shift+Tab percorre le finestre in senso inverso; la scorciatoia è riservata a ORBIT quando è in primo piano.

### Modifiche effettuate
- `src/shortcuts.rs`, `src/main.rs`, `Cargo.toml`: hook tastiera nativo su thread dedicato, limitato al primo piano e ai pianeti con finestre; filtro delle ripetizioni e rilascio dell'hook alla chiusura.
- `src/navigation.rs`, `src/controller.rs`, `src/workspace.rs`, `src/surfaces.rs`: ciclo delle finestre, volo orbitale verso la destinazione, focus all'arrivo, riduzione della vista ingrandita prima del viaggio e aggiornamento continuo della destinazione senza accodare animazioni.
- `src/webviews.rs`, `services/shell.js`, `src/scene.rs`: rilascio del focus durante il movimento, chiusura del pannello e indicazioni delle scorciatoie.
- `src/compositor.rs`, `src/webviews.rs`: creazione asincrona dei controller e degli script iniziali, conservazione dell'ultima richiesta di focus e preparazione della superficie prima di mostrarla; eliminato il blocco del rendering durante l'apertura di pagine nella vista ingrandita.
- `src/smoke.rs`: percorsi di verifica distinti per input nativo e ciclo simulato, con controlli di ciclo inverso, continuità del volo, focus e input alla destinazione, aggiornamento rapido e annullamento al cambio di progetto.
- `README.md`: comportamento e ambito della scorciatoia documentati.

### Note
- Build Rust senza warning, quattro test `shortcuts::tests` superati, formattazione Rust e sintassi `services/shell.js` verificate. Prova iniziale `cycle-simulated`: 48 controlli superati in 112,34 secondi, nessun errore; report `.runtime/native-cycle/cycle-report.json`. Il ciclo era attivato direttamente dalle azioni del driver; mouse OS, tastiera alla destinazione, PowerShell e popup usavano input e pagine reali. La successiva correzione dell'intercettazione e la prova completa nativa sono documentate nella voce precedente.
- Durante il volo gli input alla pagina sono sospesi. Navigazione manuale, cambio progetto o galassia e chiusura della destinazione interrompono il movimento.
- Ctrl+Alt+Tab e Win+Tab restano disponibili al sistema. Fuori da ORBIT e nella vista galassia Alt+Tab mantiene il comportamento Windows.

---

## 2026-10-11 — Pagine interattive sulle finestre 3D e ingrandimento animato

### Richiesta
Eliminare lo stacco tra orbita e pagina ingrandita. Rendere interattiva la finestra selezionata del pianeta corrente anche senza ingrandirla e quando è inclinata, evidenziandola con un bordo colorato.

### Modifiche effettuate
- `src/workspace.rs`, `src/controller.rs`, `src/navigation.rs`: selezione separata dall'ingrandimento, routing degli input alla pagina attiva, navigazione sullo sfondo e deselezione al cambio di progetto o alla vista galassia.
- `src/compositor.rs`, `src/webviews.rs`, `Cargo.toml`, `Cargo.lock`: pagine WebView2 tramite CompositionController e DirectComposition, input nativi e stessa istanza conservata nelle due viste; Wry resta per barra e popup.
- `src/compositor.rs`, `src/webviews.rs`: occultamento immediato della superficie alla deselezione, mantenendo il controller disponibile per completare la cattura dell'anteprima.
- `src/surfaces.rs`, `src/scene.rs`, `src/main.rs`: proiezione prospettica della superficie reale, bordo nel colore del progetto, controllo di visibilità e transizione di 0,48 secondi tra finestra inclinata e vista frontale.
- `services/shell.html`, `services/shell.js`, `services/shell.css`: controlli Ingrandisci/Riduci, indicatore della selezione e scorciatoie coerenti con il nuovo stato.
- `src/smoke.rs`, `services/content/qa.html`, `services/content/qa.js`: verifica nativa delle superfici inclinate, input reali, transizioni, popup e isolamento della selezione per pianeta.
- `README.md`: architettura e istruzioni di interazione aggiornate.

### Note
- Build Rust senza warning, formattazione e sintassi JavaScript verificate; tre test dei servizi superati. Prova nativa finale: 36 controlli superati in 38,29 secondi, nessun errore.
- Verificati click del mouse OS attraverso Winit e proiezione inversa fino agli eventi DOM trusted, comando PowerShell dalla tastiera nativa nella finestra inclinata, conservazione di DOM e input durante selezione e ingrandimento, popup con opener e postMessage senza nuove finestre persistenti. Cambio pianeta e deselezione bloccano gli input e occultano subito la superficie. Catture native DirectComposition verificate separatamente dalla scena Bevy.
- La composizione delle pagine non condivide la profondità della scena Bevy: la superficie live viene nascosta quando il controllo di visibilità rileva un'occlusione; le altre finestre mantengono le anteprime memorizzate.
- Host e scena restano Rust; i servizi applicativi riutilizzano Node.

---

## 2026-10-11 — Primo prototipo desktop Rust con navigazione 3D

### Richiesta
Portare la UX del progetto Electron in Rust, con navigazione attorno ai pianeti e finestre nello spazio. Pagine e applicazioni devono essere utilizzabili fin dalla prima versione.

### Modifiche effettuate
- `Cargo.toml`, `Cargo.lock`, `src/main.rs`: host nativo Bevy con WebView2 tramite Wry.
- `src/scene.rs`, `src/navigation.rs`, `src/workspace.rs`, `src/controller.rs`: pianeti, orbita, finestre posizionabili con Shift, persistenza spaziale e passaggio tra anteprime 3D e pagine interattive conservandone lo stato; corretti il percorso assoluto del font, le etichette occluse o rivolte all'indietro e l'apertura del launcher con Ctrl+T nell'orbita.
- `src/webviews.rs`, `services/shell.html`, `services/shell.js`, `services/shell.css`: WebView persistenti, anteprime progressive delle finestre e controlli di navigazione.
- `src/webviews.rs`: popup nativi temporanei nello stesso ambiente WebView2, collegamento con la finestra di origine e chiusura tramite Escape o annullamento.
- `src/service.rs`, `services/host.js`, `services/content/`, `package.json`, `package-lock.json`: servizi Node senza Electron per browser, Figma, VS Code, PowerShell, chat CLI, PDF, immagini e file.
- `services/code-server.js`: selezione del bundle NLS inglese compatibile per il caricamento completo del workbench VS Code.
- `services/terminal-server.js`: uso del ConPTY distribuito con node-pty tramite `useConptyDll`, con chiusura pulita del terminale e dei processi Node.
- `src/webviews.rs`, `services/host.js`, `services/navigation-addresses.js`, `services/knowledge-index.js`, `services/navigation-addresses.test.js`: esclusione degli URL di callback di autenticazione dal caricamento delle finestre persistenti, dalla persistenza e dal RAG, con test di regressione.
- `src/smoke.rs`, `services/host.test.js`: verifiche native e dei servizi.
- `projects/`: copia dei tre progetti esistenti, con nove finestre e quattro collegamenti; rimosse le due finestre callback create dal bug e ripristinati i nove identificativi originali.
- `README.md`, `.gitignore`, `start-orbit.ps1`, `start-orbit.cmd`: istruzioni e avvio della versione nativa.

### Note
- Build Rust e test dei servizi completati; confermata la generazione delle anteprime delle nove finestre.
- Verifica nativa finale sulla build aggiornata: 22 controlli superati, nessun errore; conservazione degli input del browser dopo il focus, comando PowerShell reale, catalogo provider/modelli della chat pronto senza richieste AI, workbench VS Code completo e texture reali con etichette leggibili nella cattura del progetto.
- Verificati `window.open`, `postMessage` con la finestra di origine e chiusura automatica del popup senza modificare finestre persistenti o DOM di origine. Superati regressione completa dei servizi e test di migrazione; nessun login esterno automatizzato.
- Verificata la chiusura dei servizi senza errori `AttachConsole` né processi Node orfani, sia nei test dei servizi sia nella prova nativa.
- È un primo prototipo: host e scena sono Rust, i servizi applicativi riutilizzano Node. Non include ancora tutte le funzioni avanzate della versione Electron; il progetto originale resta intatto.

---
