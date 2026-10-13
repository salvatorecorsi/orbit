# ORBIT / Rust 3D

Versione desktop Windows del workspace ORBIT con scena nativa in Rust. Bevy renderizza pianeti, finestre e camera orbitale. La pagina selezionata usa un WebView2 CompositionController e DirectComposition per restare interattiva sulla superficie inclinata nello spazio; Wry ospita la barra e i popup. Selezione e ingrandimento sono indipendenti: il passaggio fra orbita e vista ingrandita anima la stessa superficie e conserva la WebView, il DOM e i processi.

Browser, Figma, VS Code web, PowerShell, chat CLI, immagini, PDF e file usano contenuti reali. I servizi locali dell'app precedente sono riutilizzati in un sidecar Node senza Electron. Questa prova porta host, navigazione e rendering in Rust; non è ancora una riscrittura dei servizi in Rust.

## Avvio

Servono Rust, Node.js e il runtime Microsoft Edge WebView2. Le versioni bloccate di Bevy e Wry sono compatibili con il Rust 1.94.1 installato su questa macchina. VS Code serve-web viene avviato localmente alla prima apertura dell'editor; chat e terminale usano le CLI e la shell del computer.

```powershell
npm install
cargo build
.\start-orbit.ps1
```

Il launcher `start-orbit.cmd` apre l'app già compilata. I log sono in `.runtime/orbit.log` e `.runtime/orbit-errors.log`.

## Navigazione

Trascina lo sfondo per orbitare; la rotella fuori dalla pagina selezionata avvicina o allontana la camera. Clicca un pianeta per inquadrare il progetto e una sua finestra per selezionarla senza muovere la camera. Il bordo nel colore del progetto indica la pagina attiva, utilizzabile anche di sbieco: mouse, rotella e tastiera raggiungono la pagina reale. In galassia e quando cambi progetto la selezione viene rimossa.

`Ingrandisci` e `Riduci`, oppure Ctrl+Invio, animano la stessa pagina tra la superficie 3D e la vista frontale in 0,48 secondi. Escape riduce la vista ingrandita; nella vista orbitale arriva all'app selezionata. `ORBIT` e Home senza selezione inquadrano la galassia. Le frecce ruotano la camera quando nessuna pagina è selezionata. Shift più trascinamento sposta le altre finestre; nella pagina attiva Shift resta disponibile all'app.

Alt+Tab passa alla finestra successiva del pianeta corrente; Alt+Shift+Tab torna alla precedente. La camera percorre un arco attorno al pianeta e attiva la pagina all'arrivo. Le pressioni rapide aggiornano la destinazione dalla posizione corrente, senza accodare voli. Dalla vista ingrandita la pagina si riduce prima del movimento. Trascinamento, rotella e cambio di progetto interrompono il viaggio. La scorciatoia viene intercettata solo con ORBIT in primo piano e un pianeta con finestre aperte; in galassia e nelle altre applicazioni resta a Windows. Ctrl+Alt+Tab e Win+Tab mantengono il comportamento del sistema. La creazione di nuove pagine è asincrona e lascia proseguire il rendering e le transizioni.

La barra contiene progetti, elenco delle finestre e il comando `+ Nuovo`, che apre una palette flottante centrata con le scelte `Nuova finestra` e `Nuovo progetto`. Ctrl+T apre la stessa palette anche dentro una pagina; senza progetti propone subito il nome del primo progetto. Il pannello Progetti offre anche un accesso diretto a `Nuovo progetto`. Escape, la croce o un clic fuori chiudono la palette e restituiscono il focus a `+ Nuovo`. La scena e la pagina rimangono visibili dietro; gli input sottostanti sono sospesi mentre crei un elemento. Premere nuovamente Ctrl+T conserva il modulo e la bozza correnti. Browser e Figma mantengono navigazione, input e sessione WebView2; terminale ed editor restano attivi attraverso i cambi di vista. All'avvio le anteprime si popolano progressivamente con le catture delle pagine reali. Solo la pagina selezionata del pianeta corrente viene composta dal vivo quando è visibile; le altre finestre mostrano le catture memorizzate. Le pagine nascoste non producono anteprime continue. DirectComposition non condivide il buffer di profondità di Bevy: quando la superficie selezionata risulta occlusa viene nascosta e torna visibile l'anteprima 3D.

Le finestre aperte da una pagina conservano il collegamento al genitore e il suo profilo WebView2, necessario per i login in popup. Restano temporanee, seguono la pagina di partenza e si chiudono con Escape o dalla barra. I callback di autenticazione vengono esclusi dagli indirizzi ripristinati e dal contesto delle chat.

## Dati

`projects/` contiene una copia indipendente dei progetti originali, con identificativi, chat, allegati, editor e collegamenti. L'app originale non viene modificata. `workspace.info` conserva lo schema precedente; la camera 3D e gli ancoraggi delle finestre sono in `.runtime/scene.json`, le catture in `.runtime/previews/`. Il profilo WebView2 è separato dal profilo Electron; i siti richiedono il proprio accesso nella nuova app.

Se le cartelle di alcuni progetti sono state rimosse, l'app conserva una copia esatta del registro in `projects/workspace.recovery-<id>.info` e avvia i progetti ancora disponibili. Quando sono state rimosse tutte, la galassia vuota permette di creare un nuovo progetto dalla barra. Una cartella presente con `project.info` mancante o incoerente resta un errore di caricamento.

`--projects CARTELLA` sceglie i dati. `ORBIT_RUST_STATE_DIRECTORY` separa camera e catture, `ORBIT_RUST_PROFILE_DIRECTORY` separa cookie e dati WebView2, `ORBIT_RUNTIME_DIRECTORY` separa i servizi temporanei e `ORBIT_EMBEDDING_CACHE` sceglie la cache del modello locale.

## Verifica

```powershell
npm run test:services
.\target\debug\orbit-rust.exe --smoke --projects .runtime\smoke\projects
```

La verifica nativa percorre selezione, ingrandimento e ritorno all'orbita conservando DOM e input. Un click del mouse OS attraversa Winit e la proiezione inversa fino all'evento DOM trusted; la tastiera nativa esegue un comando PowerShell nella finestra inclinata, senza ingrandirla. Controlla un popup locale reale con opener, postMessage e chiusura senza nuove finestre persistenti, oltre al blocco degli input e all'occultamento immediato quando cambi pianeta o deselezioni. Verifica anche chat e catalogo modelli pronti e workbench VS Code. Non effettua login esterni né invia messaggi ai modelli AI.

La registrazione Raw Input della tastiera effettuata da Winit impediva all'hook di ricevere Alt+Tab. ORBIT rimuove questa registrazione prima di installare l'hook; gli eventi ordinari della tastiera e il Raw Input del mouse restano disponibili. L'hook viene installato entrando in primo piano su un pianeta con finestre e rimosso uscendo da questo ambito, completando prima il rilascio di Tab già intercettato. La prova nativa isolata ha ricevuto tutti i sei eventi inviati con `SendInput`, prodotto un cambio finestra e mantenuto ORBIT in primo piano.

La prova completa `--smoke`, senza simulazione del ciclo, ha superato 48 controlli in 47,32 secondi senza errori; il report conservato è `.runtime/native-cycle/native-cycle-report.json`, modalità `smoke`. Alt+Tab e Alt+Shift+Tab vengono inviati con `SendInput` e attraversano l'hook nativo, mantenendo ORBIT in primo piano. Sono verificati anche arco della camera, selezione all'arrivo, aggiornamento rapido della destinazione, annullamento al cambio di pianeta, input Unicode alla destinazione e comando PowerShell nella finestra inclinata, oltre a mouse, popup e pagine reali. La prova automatica esercita input OS iniettati; non registra un'esecuzione manuale con tastiera fisica.

La precedente prova `cycle-simulated` aveva superato 48 controlli in 112,34 secondi, attivando direttamente le azioni del driver; `ORBIT_QA_SIMULATE_CYCLE=1` resta disponibile con `--smoke` per isolare la navigazione dall'intercettazione della scorciatoia.

Superati anche i quattro test `cargo test shortcuts::tests`, `cargo fmt --check` e il controllo di sintassi di `services/shell.js`. La precedente prova delle superfici aveva superato 36 controlli e i tre test dei servizi. Le catture native che includono DirectComposition sono separate dai PNG della scena Bevy e dalle catture WebView2. Prima di usare `--smoke`, imposta cartelle dedicate per stato, profilo e progetti: la prova crea elementi nei progetti indicati.

Sono ancora da trasporre le gesture avanzate del prototipo Electron: collegamenti manuali, cambio della natura di una finestra, descrizioni/milestone e ripristino completo della cronologia e dello scorrimento browser dopo il riavvio. Sono conservati i collegamenti già presenti, le sessioni dei servizi e lo stato delle pagine durante selezione, ingrandimento e ritorno all'orbita.
