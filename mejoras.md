# Mejoras pendientes

Análisis comparativo con [semillabitcoin/broadcast-pool](https://github.com/semillabitcoin/broadcast-pool)
(Python, mismo producto: proxy Electrum que retiene la tx y la difunde por bloque/fecha/precio,
empaquetado para Umbrel y Start9). Fecha del análisis: 2026-07-13, contra su commit del 2026-06-27
(v0.3.21) y nuestro 0.3.17.

---

## Resumen estructural

| | Este proyecto | Semilla |
|---|---|---|
| Lenguaje / runtime | Rust, binario estático | Python 3 + asyncio |
| LOC de fuente | 10.728 | ~4.400 |
| Dependencias | 277 crates transitivas | 4 (`aiohttp`, `coincurve`, `pycryptodome`, `bech32`) |
| Modelo | Servidor Electrum propio | Proxy passthrough + interceptor |
| Fichero mayor | `src/electrum_server/mod.rs` (2.541) | `web/api.py` (1.700) |
| Tests | 33 | 58 |
| UI | `src/api/dashboard.html` (2.518 líneas, embebido) | SPA estática separada (html/css/js) |

**En recursos ganamos nosotros** (Rust compilado, sin GC ni intérprete, arranque en ms, poca RAM —
relevante en una Raspberry Pi). **En sencillez de código gana el suyo**, y no por poco. La causa raíz
no es el lenguaje, son tres decisiones de diseño:

1. **Passthrough en vez de reimplementar el protocolo.** Su `interceptor.py` (383 líneas) solo toca
   tres cosas — `transaction.broadcast` (retiene), `scripthash.get_history` (inyecta la pendiente) y
   la resolución de inputs. Todo lo demás lo reenvía tal cual a electrs. Nosotros **reimplementamos
   un servidor Electrum**: respondemos localmente a `server.version`, `server.banner`,
   `server.features`, `server.ping`, `blockchain.estimatefee`, `blockchain.relayfee`,
   `mempool.get_fee_histogram`, `blockchain.block.stats`, manejamos batches JSON-RPC, tablas de
   enrutado por método y TLS a mano (rustls + webpki-roots) en `indexer_transport.rs`. Cada método
   sintetizado es superficie de compatibilidad que mantenemos nosotros y que ellos tienen gratis.
2. **Menos superficie de dependencias.** 4 vs 277: su árbol se audita en una tarde, el nuestro no.
3. **Separación de responsabilidades.** Sus `proxy/`, `pool/`, `scheduler/` y `web/` tienen fronteras
   nítidas. Nuestros `electrum_server/mod.rs`, `pool/manager.rs` (1.290) y `discovery.rs` (1.293) son
   módulos-monolito que hacen varias cosas cada uno.

---

## Carencias concretas frente a Semilla

### 1. Cifrado at-rest de la tx firmada — NO EXISTE aquí (prioridad alta)

Un grep de `encrypt|aes|cipher|APP_SEED` sobre `src/**/*.rs` no devuelve **ni una línea**. Nuestra
columna `tx_hex` está en claro en el SQLite.

**El riesgo no es solo de privacidad: una tx firmada en crudo es difundible por cualquiera.** Quien
lea nuestra base de datos no puede robar fondos, pero sí puede coger una tx programada para dentro de
tres meses y difundirla ahora mismo — es decir, destruir la única función que tiene la herramienta.

**Cómo lo hacen ellos** (`src/pool/crypto.py` + `store.py`):

- Cifran **una sola columna**: `raw_hex` de `retained_txs`. El txid, fees, importes y demás metadatos
  van en claro.
- Clave derivada de `APP_SEED` con PBKDF2-HMAC-SHA256, 100.000 iteraciones, salt fijo
  `broadcast-pool-v1`. En Umbrel, `APP_SEED` lo inyecta la propia plataforma: el usuario no configura
  nada.
- `save_retained_tx()` cifra **al guardar**, no al pasar a `scheduled`. Nonce aleatorio de 12 bytes
  por cifrado; el tag GCM autentica (si manipulan la fila, al leer devuelve `[tampered]`).
- `_row_to_tx()` descifra transparentemente al leer: ningún consumidor se entera.
- `encrypt_existing_at_rest()` hace un barrido único al arrancar para cifrar filas que versiones
  antiguas dejaron en claro.
- Sin `APP_SEED`, `encrypt()` devuelve el texto tal cual: degrada limpiamente.

**Modelo de amenaza que cubre (acotado, conviene no vendérselo al usuario como más de lo que es):** la
clave vive en la misma máquina (env var del contenedor), así que **no** protege de alguien con root en
el nodo. Protege las **copias del disco**: backups de Umbrel/Start9, snapshots, un SSD que se va a
RMA, alguien que monta la tarjeta.

### 2. Fallback a Bitcoin Core: el *reloj de la cadena* — HECHO en v0.3.18

> **Estado (2026-07-14): implementado.** `src/pool/chain_health.rs` cachea qué backend sirve el reloj
> (`ChainSource::Indexer` / `BitcoinCore` / `None`); un poller cada 30 s sondea **los dos** (también
> Core con el indexador sano, para saber si el paracaídas está plegado antes de necesitarlo). Las
> puertas del scheduler pasan de `indexer_healthy()` a `chain_clock_available()`, así que con electrs
> caído las programaciones **siguen disparándose** con altura y MTP de `getblockchaininfo`. Un Core en
> IBD **no** cuenta como reloj (su punta va por detrás: dispararía contra una altura obsoleta). El
> banner del dashboard nombra el indexador, su `IP:puerto` y si las programaciones siguen vivas o en
> pausa. Se mantiene el texto original de abajo como registro del análisis.

Son dos mecanismos distintos y conviene no confundirlos:

**(a) Fallback de difusión — YA LO TENEMOS.** `src/pool/broadcaster.rs:39-49` intenta el indexer
primero y cae al RPC. Idéntico a su `_relay_raw()`. Aquí estamos empatados.

**(b) Fallback del reloj de la cadena — ESTO ES LO QUE FALTA.** Su `_node_fallback_poller()` es un
bucle cada 30 s que comprueba `if not self.upstream_connected`. Con electrs caído,
`_node_fallback_tick()` saca altura y MTP de `getblockchaininfo` y ejecuta **el ciclo completo del
scheduler** contra el nodo: difusión por bloque, por timestamp, por precio, purga y verificación de
confirmaciones. Con electrs muerto, su pool **sigue funcionando entero**.

Lo nuestro, en `src/pool/scheduler.rs:79`:

```rust
if !pool_manager.indexer_healthy() {
    return Ok(());
}
```

Si el indexador no está sano **nos salimos del tick y no pasa nada**. Nuestra altura de bloque viene
del caché que alimenta `headers.subscribe` de electrs: sin electrs no hay reloj, y sin reloj las txs
programadas por bloque o por fecha **no se disparan**. El fallback de difusión que sí tenemos nunca
llega a usarse, porque nadie decide que la tx está vencida.

Detalle que además hacen bien: cuando electrs *está* sano, siguen haciendo `health()` contra el nodo
igualmente, para que el dashboard muestre "fallback listo" en vez de descubrir que no era alcanzable
justo el día que hace falta.

Su implementación es opt-in (`BITCOIN_RPC_HOST` + user/pass o cookie) y sirve para Core, Knots o Libre
Relay: solo usa RPC estándar (`getblockchaininfo`, `sendrawtransaction`, `getrawtransaction`,
`getblockheader`).

#### Por qué esto funciona de verdad con el indexador caído (el punto clave)

La duda razonable es: "si electrs está caído, aunque sondee cada 30 s, la información estará
obsoleta". **No**, y la razón es que el poller **no consulta a electrs: consulta al nodo Bitcoin
directamente.** Son dos procesos independientes, y electrs es el que se apoya en bitcoind, no al
revés.

electrs (o Fulcrum) es solo un **índice** encima del nodo. Su única razón de existir es responder
consultas *por dirección* ("historial/saldo/UTXOs de este scripthash"), que es lo único que bitcoind
no sabe hacer. Pero la cadena — altura, cabeceras, MTP, mempool — la tiene **bitcoind, que es la
fuente autoritativa**; electrs se la copia. Cuando electrs se cae (índice corrupto, reindexado de 6 h
tras una actualización, OOM… los fallos típicos), **el nodo sigue vivo y sincronizando con la red**.
Los datos del poller no son un caché rancio: son frescos, de la fuente original.

```python
info = await self._node.health()          # getblockchaininfo contra bitcoind
height = info.get("blocks")               # altura real, ahora mismo
mtp = info.get("mediantime")              # el mediantime del nodo ES el MTP de la punta (BIP-113)
```

Con esos dos números el scheduler tiene todo lo que necesita, porque **ninguna de sus cinco
operaciones requiere el índice por dirección**:

| Operación del scheduler | Qué necesita | ¿Hace falta electrs? |
|---|---|---|
| Disparo por bloque | altura | No — `getblockchaininfo` |
| Disparo por fecha | MTP | No — `mediantime` (BIP-113) |
| Disparo por precio | CoinGecko / oráculo local | No — ni toca la cadena |
| Difundir | `sendrawtransaction` | No — es el mismo nodo al que electrs se lo reenviaría |
| Verificar confirmación | `getrawtransaction` → `getblockheader` | No — requiere `txindex=1` |

Lo único que sí se rompe sin electrs es el **proxy hacia la cartera**: Sparrow no sincroniza, no ve
saldos ni historial, no puede construir una tx nueva. Pero eso es un problema del usuario delante de
la pantalla, no de la promesa que la herramienta **ya había aceptado**: la tx programada para el
bloque 926.000 se difunde en el bloque 926.000 aunque electrs lleve dos días muerto. Esa es la promesa
que un broadcast pool no se puede permitir romper — una tx que no sale cuando toca puede ser una
liquidación en un préstamo o una migración de cartera a medias.

**Hoy, en este proyecto, esa tx NO sale.** Se queda esperando indefinidamente. Ya tenemos el
`BitcoinRpc` construido y en uso en `broadcaster.rs` para difundir; **lo que falta es enchufarlo
también como fuente de altura y MTP cuando el indexador no responde.** Es el arreglo con mejor
relación valor/esfuerzo de toda esta lista.

(El intervalo de 30 s es irrelevante en la práctica: los bloques llegan cada ~10 min, así que sondear
cada 30 s te deja como mucho medio minuto por detrás de la punta. Para difundir "en el bloque N" es
precisión de sobra.)

#### 2.b — El mensaje del dashboard cuando el indexador cae (va con el fallback, no aparte)

**Qué se ve hoy.** Con el indexador caído, el dashboard saca el banner
`sparrow_readiness_title` — literalmente **"Sparrow — check before sending"** /
*"Sparrow — revisar antes de enviar"* (`src/api/dashboard.html:1179` y `:1289`), seguido de
`sparrow_readiness_no_indexer`: *"Electrs indexer not connected (required for send confirmation)"*.
El banner se dispara desde `renderSparrowReadiness()` (`dashboard.html:1775-1789`) cuando
`status.sparrow_ready` es falso, que a su vez exige `electrum_connected` (`src/api/mod.rs:703`).

**Por qué está mal.** Es confuso y, en cuanto exista el fallback del punto 2, será además **falso**:

- Habla de *Sparrow* cuando el problema no es la cartera, es el **indexador del nodo**. El usuario va
  a revisar Sparrow, que es donde no está la avería.
- "Check before sending" no dice **qué** revisar, ni **dónde**, ni **quién** está caído.
- No nombra el servicio ni su dirección, aunque el backend **ya las tiene**: `indexer_url` viaja en el
  status (`src/api/mod.rs:407`, vía `discovery::display_indexer_url`).
- Sugiere que enviar es inseguro, cuando con el fallback a Core la app **sigue funcionando con
  normalidad**: las programaciones por bloque, fecha y precio se disparan igual y la difusión sale por
  `sendrawtransaction`. El banner asusta justo cuando debería tranquilizar.

**Texto que debe mostrar (el que pide el usuario):**

> Por favor, compruebe su indexador **electrs**/**Fulcrum** (según se esté utilizando) en
> `$IP:$Puerto` — parece estar fuera de servicio. La app, como workaround, se conectará a Bitcoin Core
> para extraer la información necesaria de la blockchain, de modo que las programaciones de las txs
> actuales se sigan llevando a cabo con datos actualizados.

**Cómo implementarlo:**

1. Separar el banner en dos estados distintos. Hoy "indexador caído" y "Sparrow mal configurado
   (Tor/proxy)" comparten un único banner y un único título; son dos avisos que no tienen nada que ver
   y deben ir por separado. El de Tor/proxy sigue siendo un aviso *sobre la cartera*; el del indexador
   es un aviso *sobre la infraestructura*.
2. Rellenar `$IP:$Puerto` con el `indexer_url` que ya llega en el status. Sin dirección concreta el
   mensaje sigue siendo tan inútil como el actual.
3. Distinguir **electrs de Fulcrum**: hoy el código los trata como intercambiables en todas partes
   (`discovery.rs`, `indexer_transport.rs`, ambos por 50001/50002) y nunca guarda cuál es. La forma
   limpia es cachear la respuesta a `server.version` de la última conexión sana — devuelve el nombre
   del software (`electrs 0.10.x` / `Fulcrum 1.9.x`) — y usar ese nombre en el mensaje. Si nunca hubo
   conexión sana, decir "electrs/Fulcrum" y punto.
4. **El tono depende del punto 2.** Este mensaje promete que "la app se conectará a Bitcoin Core para
   que las programaciones se sigan cumpliendo", y eso **hoy no es cierto**: `scheduler.rs:79` se sale
   del tick con `if !pool_manager.indexer_healthy() { return Ok(()) }` y las txs programadas **no se
   disparan**. Escribir el mensaje antes de implementar el fallback sería mentirle al usuario en el
   peor momento posible. **Orden: primero el reloj de la cadena por RPC, después el mensaje.**
5. Reflejar el estado real del fallback en el propio banner: si el RPC a Core **tampoco** responde, el
   mensaje no puede prometer un workaround que no existe — ahí sí toca un aviso duro de "las
   programaciones están paradas". De ahí que Semilla haga `health()` contra el nodo **también cuando
   electrs está sano**: para saber si el paracaídas está plegado antes de necesitarlo.

### 3. nLockTime rancio: la huella on-chain que delata la retención (prioridad media)

**El problema.** Las carteras modernas ponen `nLockTime ≈ altura actual` al firmar (anti-fee-sniping).
Eso funciona porque difunden inmediatamente: en la cadena, el locktime de una tx normal está a uno o
dos bloques de la punta.

Con un broadcast pool en medio, firmas hoy (altura 913.000) y difundes en tres meses (altura 926.000).
La tx es válida — un locktime en el pasado no invalida nada — pero aparece en la cadena con un
locktime **13.000 bloques rancio**. Ninguna cartera normal produce eso. Un observador ve
inmediatamente "esta transacción se firmó hace meses y se estuvo reteniendo": justo el patrón que la
herramienta existe para ocultar. **Retener la tx nos delata en el propio campo que debería
protegernos.**

**Su solución (experimental).** `header_faker.py`: le mienten a Liana sobre la punta de la cadena,
sirviéndole una cadena de cabeceras falsas (`prev_hash` correctamente encadenado, `merkle_root` a
ceros, `time` +600 s por bloque, `nonce` 0, sin PoW) que la sitúan en `punta_real + offset`. Liana
cree que estamos en el bloque 926.000, firma con `nLockTime = 926.000`, y BP difunde justo en ese
bloque: el locktime coincide con el momento de la difusión, indistinguible de una tx normal.

Funciona solo con Liana porque no valida PoW, solo continuidad de la cadena y hash de génesis (con
Sparrow reventaría). Y es experimental con razón: la cartera muestra una altura falsa, y cualquier tx
que construya es **no-final** hasta esa altura — si el usuario intenta difundirla por otra vía, la red
la rechaza.

**Qué hacemos nosotros (enfoque inverso).** Tratamos el `nLockTime` como una **restricción a
respetar**: `scheduler.rs:90-93` comprueba `current_height >= nlocktime` antes de difundir, y
`models.rs` tiene todo el andamiaje (`locktime_waiting`, `locktime_deferred`, `locktime_target`,
`locktime_remaining_secs`, `locktime_satisfied`) para posponer y avisar. `config.rs:26` contempla que
el usuario ponga el locktime futuro **a mano** en Liana y lo ingestemos como agenda manual.

Ambos enfoques son correctos por consenso pero resuelven cosas distintas. El nuestro garantiza que
**nunca difundimos una tx no-final** (bug real que ellos se comen: su truco *crea* txs no-finales a
propósito). El suyo consigue que **la tx no se note en la cadena**. El nuestro le pide al usuario que
se acuerde de poner el locktime a mano; el suyo lo automatiza engañando a la cartera.

**DECISIÓN (2026-07-14): bloque virtual futuro para Liana, opcional y configurable en Settings.**
Adoptamos el enfoque de cabeceras adelantadas, pero como **opción que el usuario activa**, no como
comportamiento por defecto. Sustituye al "punto medio" que se proponía aquí antes (avisar en la UI de
que el usuario pusiera el locktime a mano), que queda descartado por manual y olvidable.

**Las dos opciones conviven en Settings:**

| Opción | Qué hace | Cuándo |
|---|---|---|
| **Programar a posteriori** (actual, por defecto) | Liana firma con el locktime que quiera; la tx se retiene y el usuario le pone criterio (fecha/precio/bloque) desde el dashboard. | Comportamiento de hoy. No se toca. |
| **Bloque virtual** (nuevo, opt-in) | El usuario fija un **offset de bloques** (p. ej. +13.000). Le servimos a Liana una punta de cadena de `altura_real + offset`, Liana firma `nLockTime = altura_real + offset`, y el pool difunde **exactamente en ese bloque**. | Cuando se quiere que la tx no se distinga on-chain de una tx normal. |

Con el bloque virtual, el locktime que acaba en la cadena coincide con el bloque en que se difunde:
desaparece la huella de "firmada hace meses y retenida", que es justo lo que la sección describe como
problema.

**Por qué en este proyecto es más seguro que en el suyo.** Su `header_faker` tiene un agujero real:
crea txs **no-finales** y nada le impide difundirlas antes de tiempo. Nosotros ya tratamos el
`nLockTime` como una restricción dura — `scheduler.rs:90-93` comprueba `current_height >= nlocktime`
**antes** de difundir. Es decir: la maquinaria que hoy nos "estorbaba" frente a su enfoque se
convierte, con el bloque virtual, en la **red de seguridad** que a ellos les falta. La tx no puede
salir antes del bloque objetivo ni por error.

**Cómo implementarlo:**

1. **Solo para Liana, nunca para Sparrow.** Sparrow **valida las cabeceras y reventaría**. La puerta
   ya la tenemos construida: `SessionState::effective_source()` (`electrum_server/mod.rs:411`) ya
   distingue Liana de Sparrow en el puerto compartido. **Esta mejora depende de la anterior** (la
   columna de cartera de origen): si la detección falla, le servimos cabeceras falsas a Sparrow y le
   rompemos la sincronización. La detección deja de ser cosmética y pasa a ser crítica —
   razón de más para endurecer la heurística en vez de fiarnos de "no mandó `server.version`".
2. **El punto de inyección ya es nuestro.** `handle_headers_subscribe()` (`mod.rs:958`) ya intercepta
   el método y sirve la altura desde el caché de `PoolManager` (`pool/manager.rs:42`) en vez de
   reenviar a electrs. Aplicar el offset ahí es un cambio localizado: `height + offset` en la sesión
   de Liana.
3. **Hay que sintetizar la cadena de cabeceras por encima de la punta real**, que es la parte con
   trabajo de verdad: para las alturas entre la punta real y la virtual no existe cabecera, así que
   hay que fabricarlas encadenando `prev_hash` correctamente, `merkle_root` a ceros, `time` +600 s por
   bloque y `nonce` 0 (Liana valida continuidad de la cadena y hash de génesis, pero **no** PoW).
   Afecta también a `blockchain.block.header` / `block.headers`, que hoy se reenvían a electrs
   (`rpc/electrum.rs:165`) y para esas alturas no tendrían respuesta.
4. **Ingesta coherente.** La tx entra con `nLockTime = altura_virtual`: debe ingestarse como
   `by_block` con objetivo esa altura, no como manual. Ojo, porque hoy es justo al revés —
   `mod.rs:1402` fuerza a manual las txs de Liana con locktime por altura, precisamente porque hasta
   ahora ese locktime **no** era una intención del usuario. Con el bloque virtual activo, sí lo es, y
   esa regla debe invertirse.
5. **Avisar de lo que implica.** Con la opción activa, Liana **muestra una altura falsa** y cualquier
   tx que construya es no-final hasta esa altura: si el usuario la difunde por otra vía, la red la
   rechaza. Hay que decirlo en Settings, junto a la opción, sin letra pequeña.

**Riesgo asumido:** es el punto más invasivo de toda la lista — le estamos mintiendo a una cartera
sobre el estado de la cadena. De ahí que sea **opt-in y solo para Liana**. Ellos lo marcan como
experimental y hacen bien.

### 4. Otras cosas suyas que no tenemos (prioridad baja)

- Bóveda **NIP-44 (Nostr)** para el historial: solo descifrable con tu nsec.
- **i18n ES/EN** (README y UI).
- Ring buffer de logs saneados en memoria para un **informe de diagnóstico descargable**
  (`src/diagnostics.py`).

### 5. Lo que tenemos nosotros y ellos no

- Toda la maquinaria de **migración de carteras** (`src/migration/`).
- **CLI** con ~18 subcomandos (`pool`, `migrate`, `schedule-all`, `import-utxos`, `status`…).
- Tipado fuerte: elimina de raíz clases de bugs que en su Python solo cazan los tests. Solo 24
  `unwrap()/expect()` en todo el código.

---

## Comportamiento propio a corregir

### Clasificar la tx entrante por su nLockTime: inmediata vs. retenida (prioridad alta)

**Comportamiento actual:** interceptamos y retenemos **toda** tx que llega por
`blockchain.transaction.broadcast`, sin excepción (`electrum_server/mod.rs`, `BROADCAST_METHODS`).
No hay bifurcación: todo acaba en `pending` / `manual` esperando a que el usuario entre en la UI.

**Comportamiento correcto:** el `nLockTime` con el que la cartera firmó **ya expresa la intención del
usuario**. Hay que leerlo y bifurcar en tres casos:

| `nLockTime` de la tx | Qué significa | Qué debe hacer el pool |
|---|---|---|
| **= altura de la punta (±1)** | Valor por defecto del anti-fee-sniping. La cartera firmó de la forma normal: "envía ahora". | **Difundir directamente** (passthrough). Sin retención ni intervención del usuario. |
| **= 0 (locktime deshabilitado)** | El usuario lo desactivó **a propósito**. Es un acto deliberado: está preparando la tx para el pool. | **Retener** como programada, a la espera de que el usuario indique el criterio de envío (**precio** o **fecha/hora**). |
| **= altura/timestamp futuro** | Intención inequívoca de retraso. | **Retener** como agenda manual (es el caso de Liana que ya cubre `config.rs:26`). |

**El caso que hoy está mal es el primero.** Una tx con el locktime por defecto no ha pedido
programarse: retenerla en silencio es un comportamiento sorpresa de la peor clase, porque es
silencioso — el usuario le da a "Enviar" en Sparrow, la cartera dice que ha ido bien, y el dinero no
se mueve hasta que alguien se acuerde de entrar en el dashboard.

**El `nLockTime = 0` NO es inmediata.** Aunque técnicamente una tx sin timelock sea difundible ya,
deshabilitar el locktime en la cartera es un paso manual que nadie hace por accidente: es la señal de
"esta va al pool". De hecho **el código ya asume esto**: `PoolManager::schedule_by_price()`
(`pool/manager.rs:167`) **rechaza** poner un disparador por precio si `nLockTime > 0` —

```rust
if tx.nlocktime.is_some_and(|n| n > 0) {
    anyhow::bail!("Price trigger scheduling is only available when nLockTime is disabled (0)");
}
```

— o sea que el locktime a cero ya es, hoy, la precondición para programar por precio. La bifurcación
nueva solo tiene que ser coherente con eso.

**Ojo con el matiz de la altura:** `nLockTime = altura actual` y `nLockTime = punta` son el mismo
valor, así que el criterio hay que evaluarlo contra la punta de la cadena **en el momento de la
interceptación**, no contra un valor fijo. El margen de ±1 cubre la carrera entre el momento en que la
cartera firmó y el momento en que nos llega la tx.

**Dónde tocar:** el punto de decisión es la interceptación de `BROADCAST_METHODS` en
`src/electrum_server/mod.rs`. Hay que añadir la rama de passthrough hacia el indexer/RPC **antes de
persistir nada**. La lógica de clasificación de locktime ya existe y es reutilizable:
`PoolManager::is_locktime_satisfied()` (`pool/manager.rs:540`) distingue locktime por altura
(`< 500_000_000`) de locktime por timestamp, y compara contra la altura o el MTP.

### Identificar la cartera de origen (Liana / Sparrow) en una columna (prioridad media)

**Lo que ya está hecho — casi todo.** Esta mejora es mucho más pequeña de lo que parece, porque la
tubería entera existe ya:

- La **detección** está en `SessionState::effective_source()` (`electrum_server/mod.rs:411`), con
  tests (`:2368`). Distingue las dos carteras en el puerto Electrum compartido: Sparrow siempre manda
  `server.version` en su handshake; Liana (electrum-client, Rust) **no lo manda**. Así que
  `!saw_server_version` ⇒ Liana. Si el cliente sí se identifica, se mira el `client_name` (campo
  `wallet_label`, `:390`).
- La **columna existe**: `source_label TEXT` en `broadcast_pool` (`db/schema.rs:16`).
- Y **se persiste**: `effective_source` se guarda en el INSERT (`electrum_server/mod.rs:1416` →
  `db/mod.rs:125,136`), y se lee de vuelta en `BROADCAST_SELECT` (`db/mod.rs:13`).

**Lo que falta, que es lo que hay que hacer:**

1. **Sacarla a la UI.** Un grep de `source_label` sobre `dashboard.html` no devuelve **nada**: el dato
   se guarda en cada fila y no se muestra en ninguna parte. Hay que añadir la columna a la tabla de
   txs del dashboard, con su i18n en los dos bloques de traducción.
2. **Exponerla en la API.** Comprobar que el JSON de `/api/txs` la incluye; hoy `source_label` solo
   aparece en `api/mod.rs:270` y en un contexto distinto (ver el punto siguiente).
3. **Deshacer la sobrecarga de la columna.** En `api/mod.rs:270` se hace `source_label: req.label`:
   una **etiqueta libre que escribe el usuario** en el alta manual. O sea que la misma columna guarda
   dos cosas que no tienen nada que ver — la cartera detectada y una etiqueta humana — y la segunda
   **pisa** a la primera. Hay que separarlas: `source_wallet` (detectado, `liana`/`sparrow`/`manual`/
   `cli`) frente a `source_label` (texto del usuario). Es una `ALTER TABLE ADD COLUMN` más, en la
   línea de las que ya hay en `schema.rs:77-98`.
4. **Endurecer la heurística.** "No mandó `server.version` ⇒ es Liana" es frágil: cualquier cliente
   Electrum que tampoco se identifique se etiquetará como Liana. Mientras la atribución solo afectaba
   al modo de ingesta era tolerable; en cuanto se muestra al usuario como un hecho, conviene guardar
   también el `wallet_label` crudo y, cuando no haya identificación fiable, mostrar `desconocido` en
   vez de adivinar.

**Por qué importa más de lo que parece:** la cartera de origen ya condiciona el comportamiento del
pool (una tx de Liana con nLockTime por altura se ingesta como manual en vez de `by_block`,
`mod.rs:1402`). Si eso cambia el resultado, el usuario tiene que **poder verlo**; hoy es una decisión
invisible tomada a sus espaldas. Además se acopla con la mejora nº 4 (clasificación por nLockTime):
las dos ramifican según de dónde viene la tx, y deben leer la misma fuente de verdad.

---

## Descartado explícitamente

**Refactor estructural — NO se hace.** Decisión del 2026-07-14: se mantiene el código tal cual. Queda
aquí anotado para no volver a proponerlo:

- ~~Convertir `electrum_server` en passthrough real~~ (bajaría miles de líneas, pero es reescribir el
  núcleo del proyecto).
- ~~Partir los módulos-monolito~~ (`electrum_server/mod.rs` 2.541, `pool/manager.rs` 1.290,
  `discovery.rs` 1.293).
- ~~Sacar el dashboard del HTML embebido~~ (2.518 líneas).

Sigue siendo razonable **subir la cobertura de tests** (33 vs sus 58), que no exige tocar la
estructura.
