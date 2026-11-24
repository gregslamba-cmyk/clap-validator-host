================================================================================
CLAP-VALIDATOR-HOST — BANCO DI COLLAUDO UNIVERSALE & GUI DESIGNER
================================================================================

clap-validator-host e' lo strumento di collaudo, stress-test e caratterizzazione
elettroacustica automatizzata per tutti i plugin audio CLAP dell'ecosistema
Analog-Vibe. Consente di misurare con precisione da laboratorio la risposta
in frequenza, la distorsione armonica (THD), le curve di saturazione dinamica,
la latenza esatta, l'assenza di crash o valori NaN/Inf e include un GUI Layout
Designer interattivo Drag & Drop.


--------------------------------------------------------------------------------
1. ARCHITETTURA DI CARICAMENTO DINAMICO (ABI C NATIVA)
--------------------------------------------------------------------------------

Lo strumento opera come un vero host audio professionale a basso livello,
interfacciandosi direttamente con le librerie condivise compilate (.so su Linux,
.dylib su macOS, .dll su Windows) senza intermediari:

1. Caricamento tramite libloading:
   Apre il file binario compilato e risolve il simbolo di entry point ufficiale
   clap_entry (clap_plugin_entry).
2. Interrogazione del Factory CLAP:
   Richiede il factory standard CLAP_PLUGIN_FACTORY_ID (clap_plugin_factory) e
   crea l'istanza del plugin tramite factory.create_plugin().
3. Negoziazione delle Estensioni Ufficiali CLAP:
   - CLAP_EXT_AUDIO_PORTS: interroga numero di porte e canali supportati
     (da Mono 1ch a 7.1 Surround 8ch per audio principale e sidechain).
   - CLAP_EXT_PARAMS: enumera tutti i parametri esposti, estraendo info.id,
     info.name, info.min_value, info.max_value, info.default_value e info.cookie.
   - CLAP_EXT_LATENCY: interroga l'estensione di latenza per verificare che la
     latenza dichiarata all'host coincida al singolo campione con il ritardo effettivo.
   - CLAP_EXT_THREAD_CHECK: valida il rispetto del threading model audio/GUI.


--------------------------------------------------------------------------------
2. PROTOCOLLI DI TEST & MISURE ELETTROACUSTICHE
--------------------------------------------------------------------------------

Il banco sottopone ciascun plugin a una serie di test deterministici rigorosi:

2.1 Caratterizzazione della Saturazione Reale (-12 VU -> +18 VU & Max Drive)
Il plugin viene sollecitato su 6 livelli di ampiezza calibrati:
- Livello 1 (-12 VU / -30 dBFS): comportamento nominale a basso segnale (zona ultra-lineare).
- Livello 2 (0 VU / -18 dBFS): livello di lavoro nominale da studio analogico.
- Livello 3 (+6 VU / -12 dBFS): inizio della compressione morbida e armoniche musicali.
- Livello 4 (+12 VU / -6 dBFS): saturazione profonda e distorsione di nucleo.
- Livello 5 (+18 VU / 0 dBFS): limite superiore prima del clipping digitale.
- Livello 6 (Max Drive / +999): stress test per verificare che overdrive estremi
  vengano gestiti senza overflow numerico, esplosione dei buffer o generazione di NaN.

2.2 Analisi Spettrale THD e Armoniche H2-H7
- Calcolo del THD% (Total Harmonic Distortion) tramite algoritmo di Goertzel
  ad alta precisione sulla frequenza fondamentale di test (1000.0 Hz).
- Isolamento ed estrazione dei contributi armonici individuali:
  * H2 (2ª armonica / 2000 Hz) e H4: armoniche pari tipiche di valvole asimmetriche e transistor Class A.
  * H3 (3ª armonica / 3000 Hz) e H5: armoniche dispari generate dalla saturazione dei trasformatori a nucleo ferroso.
  * Valori registrati in dBc (decibel rispetto alla fondamentale).

2.3 Sweep dei Parametri & Test Anti-Zipper
- Step statici: verifica del comportamento del plugin con parametri impostati
  al minimo, al default e al massimo.
- Automazione dinamica continua: invio di eventi CLAP_EVENT_PARAM_VALUE a ogni
  blocco audio per testare la risposta degli interpolatori di parametro
  (realtime_controls), accertando la totale assenza di click, pop o zipper noise.

2.4 Stress-Test con Dimensioni di Blocco Irregolari
Il processore viene testato con buffer arbitrari di dimensioni variabili:
1, 32, 64, 128, 256, 512, 1024, 2048 campioni, verificando che la continuita'
della fase e lo stato interno dei filtri rimangano perfettamente stabili.


--------------------------------------------------------------------------------
3. I 6 SEGNALI SINTETICI CALIBRATI
--------------------------------------------------------------------------------

Il validatore genera 6 diverse tipologie di segnali audio di riferimento:
1. Sine: onda sinusoidale pura a 1000 Hz finestrata Hann per misure di THD e Bode.
2. Sawtooth: onda a dente di sega ricca di armoniche pari e dispari per risonanze.
3. Triangle: onda triangolare con decadimento armonico rapido (1/n^2).
4. Square: onda quadra per verificare la risposta ai fronti ripidi e l'anti-aliasing.
5. Impulse: impulso unitario (delta di Dirac) per la misura della risposta all'impulso
   e della latenza esatta in campioni.
6. Dynamic Burst: inviluppo sonoro che modula continuamente tra -30 dBFS e -3 dBFS
   per misurare i tempi di attacco, rilascio e recupero dei compressori.


--------------------------------------------------------------------------------
4. REPORT GENERATI & GRAFICI SVG VETTORIALI
--------------------------------------------------------------------------------

Ad ogni ciclo di validazione, clap-validator-host produce una serie completa
di artefatti tecnici:

1. Dati Grezzi CSV (target/validation_data/<plugin>_validation_<timestamp>.csv):
   Tabella contenente per ogni iterazione: block_size, param_name, param_value,
   input_rms, output_rms, gain_db, thd_percent, bode_gain_db, bode_phase_deg,
   latency_samples, nan_detected.

2. Report HTML Interattivo (target/validation_report.html):
   Pagina HTML auto-contenuta con tabelle di riepilogo e grafici vettoriali SVG:
   - Diagramma di Bode (Risposta in Frequenza & Fase): ampiezza in dB e fase in gradi.
   - Spettro Armonico H2-H7: andamento delle armoniche in funzione del livello di ingresso.
   - Curva di Trasferimento Input/Output: visualizzazione grafica della curva di compressione.
   - Inviluppo Transiente / Gain Reduction: risposta temporale del circuito dinamico.


--------------------------------------------------------------------------------
5. GUI LAYOUT DESIGNER DRAG & DROP
--------------------------------------------------------------------------------

Nel report HTML target/validation_report.html e' integrato il GUI Layout Designer,
un editor visuale in ambiente web/canvas che permette a sviluppatori e sound
designer di creare o riorganizzare il layout grafico dei plugin Analog-Vibe:

- Canvas Vettoriale Interattivo (820 x 360 pixel):
  Visualizza in tempo reale tutti i controlli del plugin (knob, toggle, switch,
  meter, display spettrale) come elementi posizionabili e ridimensionabili.
- Griglia con Snap Magnetico:
  Selettore di snap configurabile a 4px, 8px o 16px per un allineamento perfetto.
- Personalizzazione dei Colori:
  Color picker integrati per il colore di sfondo (Background Hex) e il colore
  accento primario (Accent Hex).
- Maniglie di Ridimensionamento:
  Ogni widget puo' essere trascinato o ridimensionato direttamente con il mouse.
- Esportazione Istantanea:
  Cliccando sul pulsante "Esporta / Copia gui_layout.json", il designer genera
  il blocco JSON conforme alla specifica GuiLayoutSpec di greg_gui_core, pronto
  per essere salvato come gui_layout.json accanto al binario del plugin.


--------------------------------------------------------------------------------
6. GUIDA AI COMANDI
--------------------------------------------------------------------------------

Compilazione dello strumento:
  cargo build --release -p clap-validator-host

Collaudo di un singolo plugin:
  ./target/release/clap-validator-host target/release/libgreg_76.so

Scansione automatica e validazione di tutti i plugin presenti in target/release/:
  ./target/release/clap-validator-host --update-docs

Apertura del report generato:
  xdg-open target/validation_report.html  # Su Linux
  open target/validation_report.html      # Su macOS
================================================================================
