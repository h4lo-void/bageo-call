//! Whether a Whitelist caller's words name a real emergency (the urgent alert,
//! and the urgent call when that is on), and in which language they speak, for
//! the nine languages.
//!
//! A sentence is urgent when it names an emergency situation (rushed to the ER,
//! a collapse or stroke, a crash, a fire / gas leak / flood, someone missing, a
//! death, a robbery or break-in, someone trapped or stranded abroad, something
//! bad at home, an emergency at work, a child hurt at school), or says outright
//! that it is urgent, or asks for the owner at once; and nothing in the same
//! sentence says it is not urgent (no hurry, a joke, the news, a drill). A
//! situation does not count either when the sentence says it is over or minor
//! ("nothing serious", "already discharged") or past ("last year", unless it also
//! says "just now"). Bare words (hospital, accident, right now, 大変, 빨리) count
//! for nothing on their own.

use crate::locale::Lang;
use std::sync::OnceLock;

/// The cue phrases of one language (Chinese written simplified: the text is
/// folded to simplified before matching). A trailing * matches word starts.
struct Cues {
    /// Emergency situations.
    events: &'static [&'static str],
    /// Urgent on their own.
    urge: &'static [&'static str],
    /// Asking for the owner…
    ask: &'static [&'static str],
    /// …at once (both in the sentence count as urgent).
    at_once: &'static [&'static str],
    /// The sentence is not urgent at all (no hurry, joke, news, drill, fiction).
    calm: &'static [&'static str],
    /// The situation is over or minor.
    fine: &'static [&'static str],
    /// The situation is in the past…
    past: &'static [&'static str],
    /// …unless it happened just now.
    now: &'static [&'static str],
}

const ZH: Cues = Cues {
    events: &[
        "送医", "送急诊", "去急诊", "进急诊", "在急诊", "急诊室", "被送进", "送到医院", "进医院了", "住院了", "急救", "抢救", "昏倒", "晕倒",
        "昏过去", "晕过去", "中风", "心脏病发", "没有呼吸", "没呼吸", "救护车", "加护病房", "重症监护", "紧急开刀", "摔倒了", "跌倒了",
        "车祸", "被车撞", "撞车", "出事了", "出事情了", "出大事", "发生意外", "出意外", "受重伤", "受伤", "摔伤", "撞伤", "烫伤",
        "失火", "火灾", "着火", "烧起来", "瓦斯外泄", "瓦斯漏", "煤气泄漏", "燃气泄漏", "闻到瓦斯", "淹水", "淹大水", "水淹", "爆炸", "浓烟",
        "走失", "走丢", "不见了", "失踪", "找不到他", "找不到她", "找不到人", "找不到孩子", "找不到小孩", "一直没回家", "到现在还没回家",
        "过世", "去世", "往生", "病危", "快不行了", "没有心跳", "断气",
        "被抢", "抢劫", "遭小偷", "被偷了", "闯空门", "有人闯进", "撬门", "报警了", "已经报警", "警察来了", "被打了", "绑架",
        "被困", "困在", "卡在电梯", "关在电梯", "受困", "迷路了", "下不了山",
        "护照丢", "护照不见", "护照被偷", "钱包丢", "钱包被偷", "都丢了", "全丢了", "回不了国", "回不了家", "被扣留",
        "家里出事", "宕机", "当机", "系统挂了", "整个挂了", "系统崩溃", "服务器挂", "网站挂了", "停摆", "炸锅", "工安意外", "工安事故", "工地出事",
        "学校打来", "老师打来",
    ],
    urge: &[
        "紧急", "有急事", "急事", "十万火急", "救命", "快来", "快过来", "快点过来", "赶快过来", "赶紧过来", "快回来", "赶快回来",
        "赶紧回来", "你马上回", "叫他马上", "要他马上", "请他马上", "让他马上", "叫他立刻", "请他立刻", "马上找他", "立刻找他", "马上叫他",
        "马上接电话", "赶快接电话", "快点接", "快接电话", "马上联络他", "找本人",
    ],
    ask: &[],
    at_once: &[],
    calm: &[
        "不急", "不用急", "不着急", "不用着急", "不赶", "没急事", "没有急事", "没什么急事", "有空再", "有空的时候", "慢慢来",
        "哈哈", "笑死", "开玩笑", "饿死", "饿到", "饿得", "新闻", "报导", "报道", "演习", "演练", "电影", "电视剧",
    ],
    fine: &["没事", "没什么事", "没啥事", "没什么大碍", "不严重", "已经好了", "已经出院", "出院了", "没出事", "还好没"],
    past: &["上周", "上礼拜", "上星期", "上次", "上回", "去年", "前年", "以前", "之前", "小时候", "那时候", "几年前", "前几年", "忌日"],
    now: &["刚刚", "刚才", "刚被", "现在", "正在"],
};

const JA: Cues = Cues {
    events: &[
        "倒れて", "倒れた", "倒れま", "救急車", "救急搬送", "搬送され", "病院に運ばれ", "意識がない", "意識不明", "脳卒中", "心筋梗塞", "心臓発作",
        "危篤", "集中治療室", "事故に遭", "事故にあ", "事故った", "交通事故", "はねられ", "ひかれ", "手術中", "緊急手術",
        "火事", "燃えて", "ガス漏れ", "ガスが漏れ", "浸水", "水浸し", "爆発", "煙が",
        "行方不明", "迷子", "いなくなっ", "どこにもいない", "帰ってこな",
        "亡くなっ", "亡くなり", "死んだ", "もう長くない",
        "空き巣", "泥棒", "強盗", "ひったくり", "警察を呼", "警察に通報", "通報し",
        "閉じ込められ", "遭難", "道に迷", "取り残され", "盗まれ", "財布をなく", "財布を落と",
        "大変なこと", "システムが落ち", "サーバーが落ち", "サーバーダウン", "システム障害", "障害が発生", "全部止まっ", "労災", "現場で事故",
        "学校から電話", "けがをし", "ケガをし", "怪我をし", "骨折",
    ],
    urge: &["緊急", "至急", "急ぎの用", "急用", "大至急", "すぐ繋いで", "すぐつないで", "すぐ来て", "すぐに来て", "今すぐ", "助けて"],
    ask: &[],
    at_once: &[],
    calm: &[
        "急ぎじゃない", "急ぎではない", "急がない", "急がなくて", "急ぎません", "時間あるとき", "時間がある時", "お手すきの", "暇なとき", "ゆっくりで",
        "ニュース", "訓練", "ドラマ", "映画", "冗談", "死にそう", "お腹すいて", "腹減",
    ],
    fine: &["大丈夫", "大したことない", "たいしたことない", "退院し"],
    past: &["先週", "去年", "昨年", "前に", "昔", "この前", "以前", "命日"],
    now: &["さっき", "たった今", "今すぐ", "いま", "現在"],
};

const KO: Cues = Cues {
    events: &[
        "쓰러지", "쓰러졌", "구급차", "응급실", "실려갔", "실려 갔", "의식이 없", "뇌졸중", "심장마비", "중환자실", "위독",
        "교통사고", "사고가 났", "사고 났", "사고를 당", "차에 치", "수술 중",
        "불이 났", "불났", "화재", "가스가 새", "가스 새", "가스 누출", "물에 잠겼", "침수", "폭발",
        "실종", "길을 잃", "없어졌", "안 들어오", "찾을 수가 없", "어디에도 없",
        "돌아가셨", "돌아가시", "가망이 없",
        "도둑이 들", "도둑맞", "강도가", "강도를", "강도 당", "소매치기", "경찰에 신고", "신고했", "경찰 불렀",
        "갇혀", "갇혔", "고립", "조난",
        "여권이랑 지갑", "여권을 잃", "지갑을 잃", "다 잃어버", "도난", "돌아갈 방법이 없", "돌아갈 수가 없",
        "큰일 났", "큰일이 났", "서버가 다운", "서버 다운", "시스템이 다운", "시스템 장애", "장애가 발생", "전부 멈췄", "산업재해", "현장에서 사고",
        "학교에서 전화", "다쳐서", "다쳤", "골절",
    ],
    urge: &["긴급", "급한 일", "급한 용", "급합니다", "급해요", "응급", "빨리 바꿔", "빨리 와", "당장", "살려 주세요", "살려줘"],
    ask: &[],
    at_once: &[],
    calm: &[
        "급하지 않", "안 급해", "급한 거 아니", "급한 건 아니", "시간 날 때", "천천히", "편할 때",
        "뉴스", "훈련", "드라마", "영화", "농담", "배고파", "죽겠다",
    ],
    fine: &["괜찮아", "괜찮습니다", "별일 아니", "별거 아니", "퇴원했"],
    past: &["지난주", "작년", "예전에", "저번에", "지난번", "기일"],
    now: &["방금", "지금", "아까", "당장"],
};

const EN: Cues = Cues {
    events: &[
        "collapsed", "passed out", "ambulance", "to the er", "in the er", "the emergency room", "taken to the hospital", "rushed to the hospital",
        "had a stroke", "heart attack", "unconscious", "not breathing", "icu", "on life support",
        "car accident", "car crash", "in an accident", "been an accident", "hit by a car", "got hit by",
        "on fire", "house is burning", "fire department", "smell gas", "gas leak", "flooding", "flooded", "explosion",
        "wandered off", "is missing", "went missing", "gone missing", "can't find her", "can't find him",
        "passed away", "just died", "has died", "is dying", "won't make it", "not going to make it",
        "broke into", "break in", "got robbed", "been robbed", "was robbed", "mugged", "called the police", "called the cops", "called 911", "calling 911",
        "stuck in an elevator", "stuck in the elevator", "trapped", "stranded", "lost on the",
        "got stolen", "was stolen", "were stolen", "lost my passport", "lost my wallet", "no way home",
        "something bad happened", "something terrible happened", "something happened at home",
        "system is down", "system went down", "servers are down", "server is down", "site is down", "production is down", "production is stopped",
        "production has stopped", "outage", "workplace accident", "accident at the site", "hurt at work",
        "school called", "school just called", "nurse called", "hit his head", "hit her head", "broke his arm", "broke her arm", "hurt at school",
    ],
    urge: &["emergency", "it's urgent", "it is urgent", "this is urgent", "very urgent", "urgently", "something urgent", "urgent matter", "life or death"],
    ask: &["need him", "need to reach him", "need to talk to him", "need to speak to him", "get him", "put him on", "tell him to", "have him call", "find him", "reach him"],
    at_once: &["right now", "right away", "immediately", "asap", "now"],
    calm: &[
        "not urgent", "no rush", "no hurry", "whenever you", "when you get a chance", "no big deal", "just kidding", "kidding", "lol", "haha",
        "news", "drill", "movie", "dying of hunger", "starving",
    ],
    fine: &["nothing serious", "i'm fine", "all good", "doing fine", "doing ok", "discharged", "recovered"],
    past: &["last week", "last year", "last month", "years ago", "a while ago", "the other day", "anniversary"],
    now: &["just", "right now", "at the moment", "currently"],
};

const ES: Cues = Cues {
    events: &[
        "se desmayo", "desmayada", "desmayado", "ambulancia", "a urgencias", "en urgencias", "ingresar al hospital", "acaba de ingresar",
        "esta ingresado", "esta ingresada", "lo ingresaron", "la ingresaron", "un derrame", "un infarto", "no respira", "inconsciente", "en la uci", "terapia intensiva",
        "accidente de coche", "accidente de carro", "accidente de trafico", "tuvo un accidente", "hubo un accidente", "atropello", "atropellado", "atropellada", "choque",
        "lo estan operando", "la estan operando",
        "se esta quemando", "se quema", "incendio", "bomberos", "fuga de gas", "huele a gas", "se inundo", "inundacion", "explosion",
        "no encontramos", "no lo encontramos", "no la encontramos", "se perdio", "desaparecido", "desaparecida", "desaparecio",
        "fallecio", "acaba de fallecer", "murio", "se esta muriendo", "muy grave", "esta grave",
        "nos robaron", "me robaron", "me asaltaron", "asalto", "entraron a robar", "llamamos a la policia", "llame a la policia",
        "atrapado", "atrapada", "encerrado", "encerrada", "nos perdimos",
        "perdi el pasaporte", "perdi la cartera", "no tengo como volver",
        "algo muy grave", "algo grave", "se cayo todo", "se cayo el sistema", "se cayo el servidor", "sistema caido", "servidor caido",
        "produccion parada", "produccion esta parada", "accidente laboral",
        "llamaron del colegio", "llamaron de la escuela", "se rompio", "se golpeo la cabeza", "se lastimo", "se hizo dano",
    ],
    urge: &["emergencia", "es urgente", "muy urgente", "urgentemente", "auxilio", "socorro"],
    ask: &["dile que", "digale que", "que venga", "que se venga", "comuniquelo", "pasamelo", "pasemelo", "que me llame", "necesito hablar con el"],
    at_once: &["ahora mismo", "ya", "enseguida", "inmediatamente", "urgente"],
    calm: &[
        "no es urgente", "nada urgente", "no hay prisa", "sin prisa", "no corre prisa", "cuando puedas", "cuando tengas tiempo", "cuando pueda",
        "noticias", "simulacro", "pelicula", "novela", "es broma", "jaja", "me muero de hambre",
    ],
    fine: &["nada grave", "no es nada", "estoy bien", "ya esta bien", "ya salio", "le dieron el alta"],
    past: &["la semana pasada", "el ano pasado", "el mes pasado", "hace anos", "el otro dia", "aniversario"],
    now: &["acaba de", "acabo de", "ahora", "ahorita", "en este momento"],
};

const FR: Cues = Cues {
    events: &[
        "malaise", "aux urgences", "pompiers", "samu", "emmene a l hopital", "emmenee a l hopital", "l emmenent", "hospitalise", "hospitalisee",
        "avc", "crise cardiaque", "inconscient", "inconsciente", "respire plus", "reanimation",
        "accident de voiture", "eu un accident", "il y a eu un accident", "renverse par", "renversee par", "au bloc",
        "il y a le feu", "au feu", "pris feu", "prend feu", "incendie", "fuite de gaz", "sent le gaz", "inonde", "inondee", "inondation", "degat des eaux", "explosion",
        "retrouve plus", "retrouve pas", "s est perdu", "s est perdue", "a disparu", "porte disparu",
        "vient de mourir", "est mort", "est morte", "decede", "decedee", "en train de mourir", "s en sortir",
        "cambriol*", "braque", "braques", "agresse", "agressee", "appele la police", "appele les flics",
        "coince*", "bloque dans", "bloquee dans", "perdus en montagne", "enferme*",
        "vole mon passeport", "vole mon portefeuille", "perdu mon passeport", "perdu mon portefeuille", "peux pas rentrer", "peux plus rentrer",
        "quelque chose de grave", "a l arret", "en panne", "accident du travail",
        "l ecole a appele", "l ecole vient d appeler", "s est casse", "s est cogne", "s est blesse*", "est tombee", "est tombe",
    ],
    urge: &["c est urgent", "tres urgent", "une urgence", "c est une urgence", "urgence", "au secours", "a l aide"],
    ask: &["passez le moi", "passe le moi", "dis lui de", "dites lui de", "qu il rentre", "qu il vienne", "qu il me rappelle", "j ai besoin de lui"],
    at_once: &["tout de suite", "immediatement", "maintenant", "vite", "d urgence"],
    calm: &[
        "pas urgent", "rien d urgent", "pas presse", "rien ne presse", "quand tu peux", "quand vous pouvez", "quand tu auras", "quand vous aurez",
        "infos", "journal", "exercice", "film", "je rigole", "je plaisante", "mdr", "meurs de faim",
    ],
    fine: &["rien de grave", "je vais bien", "il va bien", "elle va bien", "est sorti de l hopital", "est sortie de l hopital"],
    past: &["la semaine derniere", "l annee derniere", "l an dernier", "l an passe", "il y a des annees", "l autre jour", "anniversaire"],
    now: &["vient de", "viens de", "maintenant", "en ce moment", "tout de suite"],
};

const DE: Cues = Cues {
    events: &[
        "umgekippt", "zusammengebrochen", "bewusstlos", "rettungswagen", "krankenwagen", "notaufnahme", "notarzt", "schlaganfall", "herzinfarkt",
        "atmet nicht", "intensivstation", "ins krankenhaus gebracht", "eingeliefert",
        "autounfall", "unfall gehabt", "hatte einen unfall", "einen unfall", "angefahren", "wird operiert", "wird gerade operiert",
        "brennt", "feuer", "feuerwehr", "riecht nach gas", "gasleck", "uberschwemmt", "unter wasser", "explosion",
        "verschwunden", "vermisst", "hat sich verlaufen", "haben uns verlaufen", "nicht nach hause gekommen",
        "gestorben", "verstorben", "im sterben", "stirbt",
        "eingebrochen", "einbruch", "uberfallen", "ausgeraubt", "beklaut", "polizei gerufen", "polizei angerufen",
        "im aufzug fest", "im fahrstuhl fest", "stecke fest", "stecken fest", "sitze fest", "sitzen fest", "eingesperrt", "eingeschlossen",
        "geklaut", "gestohlen", "pass verloren", "geldbeutel verloren", "nicht mehr nach hause",
        "was schlimmes passiert", "etwas schlimmes passiert", "ist was passiert",
        "system ausgefallen", "system ist ausgefallen", "server ausgefallen", "server ist down", "system ist down", "produktion steht", "arbeitsunfall", "betriebsunfall",
        "schule hat angerufen", "verletzt", "gebrochen",
    ],
    urge: &["notfall", "es ist dringend", "sehr dringend", "dringend sprechen", "dringend erreichen"],
    ask: &["muss ihn", "muss mit ihm", "verbinden sie mich", "holen sie ihn", "sag ihm er soll", "sagen sie ihm er soll", "er soll", "er muss"],
    at_once: &["sofort", "jetzt", "dringend"],
    calm: &[
        "nicht dringend", "keine eile", "eilt nicht", "wenn du zeit hast", "wenn sie zeit haben", "in ruhe",
        "nachrichten", "ubung", "probealarm", "film", "serie", "spass", "sterbe vor hunger",
    ],
    fine: &["nichts schlimmes", "alles gut", "geht mir gut", "geht ihm gut", "geht ihr gut", "entlassen"],
    past: &["letzte woche", "letztes jahr", "letzten monat", "vor jahren", "neulich", "geburtstag", "todestag"],
    now: &["gerade", "eben", "jetzt", "sofort"],
};

const PT: Cues = Cues {
    events: &[
        "desmaiou", "ambulancia", "pronto socorro", "levado para o hospital", "levada para o hospital", "levado pro hospital", "levada pro hospital",
        "internado", "internada", "avc", "infarto", "nao esta respirando", "inconsciente", "uti",
        "acidente de carro", "sofreu um acidente", "houve um acidente", "teve um acidente", "atropelado", "atropelada", "bateu o carro", "em cirurgia",
        "pegando fogo", "pegou fogo", "incendio", "bombeiros", "vazamento de gas", "cheiro de gas", "alagou", "alagada", "enchente", "explosao",
        "sumiu", "desaparecido", "desaparecida", "se perdeu", "nao achamos", "nao encontramos", "nao encontra",
        "faleceu", "acabou de falecer", "morreu", "esta morrendo", "ta morrendo", "nao vai resistir",
        "assaltaram", "assalto", "fomos assaltados", "roubaram", "arrombaram", "chamei a policia", "chamamos a policia",
        "preso no elevador", "presa no elevador", "presos no elevador", "ficou preso", "ficou presa", "perdidos na",
        "perdi o passaporte", "perdi a carteira", "nao tenho como voltar",
        "coisa seria", "coisa grave", "algo grave", "caiu inteiro", "caiu tudo", "sistema caiu", "servidor caiu", "producao parou", "fora do ar", "acidente de trabalho",
        "a escola ligou", "ligaram da escola", "quebrou o braco", "quebrou a perna", "se machucou", "bateu a cabeca",
    ],
    urge: &["emergencia", "e urgente", "muito urgente", "urgencia", "socorro"],
    ask: &["fala pra ele", "diz pra ele", "diga a ele", "pede pra ele", "preciso falar com ele", "chama ele"],
    at_once: &["agora mesmo", "ja", "urgente", "imediatamente", "agora"],
    calm: &[
        "nao e urgente", "nada urgente", "sem pressa", "nao tem pressa", "quando puder", "quando der",
        "jornal", "noticia", "simulado", "simulacao", "filme", "novela", "brincadeira", "kkk", "morrendo de fome",
    ],
    fine: &["nada grave", "nao foi nada", "to bem", "estou bem", "ta tudo bem", "teve alta", "recebeu alta"],
    past: &["semana passada", "ano passado", "mes passado", "faz anos", "ha anos", "outro dia", "aniversario"],
    now: &["acabou de", "agora", "neste momento"],
};

const ALL_CUES: [&Cues; 8] = [&ZH, &JA, &KO, &EN, &ES, &FR, &DE, &PT];

/// One cue, folded like the text it is matched against.
struct Cue {
    /// CJK / kana / hangul: matched without spaces; else whole words.
    compact: bool,
    pattern: String,
}

struct Compiled {
    events: Vec<Cue>,
    urge: Vec<Cue>,
    ask: Vec<Cue>,
    at_once: Vec<Cue>,
    calm: Vec<Cue>,
    fine: Vec<Cue>,
    past: Vec<Cue>,
    now: Vec<Cue>,
}

fn compile(list: &[&str]) -> Vec<Cue> {
    list.iter()
        .map(|raw| {
            let (raw, prefix) = match raw.strip_suffix('*') {
                Some(r) => (r, true),
                None => (*raw, false),
            };
            let compact = raw.chars().any(is_wide);
            let pattern = if compact {
                fold_compact(raw)
            } else {
                let spaced = fold_spaced(raw);
                if prefix {
                    spaced.trim_end().to_string()
                } else {
                    spaced
                }
            };
            Cue { compact, pattern }
        })
        .collect()
}

fn compiled() -> &'static [Compiled] {
    static C: OnceLock<Vec<Compiled>> = OnceLock::new();
    C.get_or_init(|| {
        ALL_CUES
            .iter()
            .map(|c| Compiled {
                events: compile(c.events),
                urge: compile(c.urge),
                ask: compile(c.ask),
                at_once: compile(c.at_once),
                calm: compile(c.calm),
                fine: compile(c.fine),
                past: compile(c.past),
                now: compile(c.now),
            })
            .collect()
    })
}

/// CJK ideographs, kana and hangul: phrases without spaces between words.
fn is_wide(c: char) -> bool {
    (c as u32) >= 0x2E80
}

/// Traditional → simplified for the characters the cues and common speech use.
const T2S: &[(char, char)] = &[
    ('醫', '医'), ('診', '诊'), ('車', '车'), ('禍', '祸'), ('緊', '紧'), ('搶', '抢'), ('報', '报'), ('災', '灾'), ('氣', '气'), ('見', '见'),
    ('過', '过'), ('闖', '闯'), ('傷', '伤'), ('掛', '挂'), ('當', '当'), ('機', '机'), ('證', '证'), ('錢', '钱'), ('個', '个'), ('電', '电'),
    ('話', '话'), ('這', '这'), ('們', '们'), ('來', '来'), ('馬', '马'), ('請', '请'), ('讓', '让'), ('關', '关'), ('學', '学'), ('從', '从'),
    ('風', '风'), ('發', '发'), ('煙', '烟'), ('燒', '烧'), ('護', '护'), ('進', '进'), ('斷', '断'), ('剛', '刚'), ('樓', '楼'), ('裡', '里'),
    ('裏', '里'), ('時', '时'), ('開', '开'), ('門', '门'), ('號', '号'), ('習', '习'), ('練', '练'), ('聞', '闻'), ('導', '导'), ('沒', '没'),
    ('趕', '赶'), ('點', '点'), ('連', '连'), ('絡', '络'), ('聯', '联'), ('現', '现'), ('對', '对'), ('嚴', '严'), ('險', '险'), ('喪', '丧'),
    ('丟', '丢'), ('會', '会'), ('經', '经'), ('後', '后'), ('盜', '盗'), ('竊', '窃'), ('實', '实'), ('驗', '验'), ('員', '员'), ('廠', '厂'),
    ('業', '业'), ('統', '统'), ('線', '线'), ('據', '据'), ('庫', '库'), ('戶', '户'), ('頭', '头'), ('腦', '脑'), ('歲', '岁'), ('說', '说'),
    ('麼', '么'), ('嗎', '吗'), ('為', '为'), ('還', '还'), ('處', '处'), ('單', '单'), ('區', '区'), ('遠', '远'), ('邊', '边'), ('給', '给'),
    ('帶', '带'), ('難', '难'), ('種', '种'), ('應', '应'), ('該', '该'), ('認', '认'), ('識', '识'), ('訴', '诉'), ('煩', '烦'), ('媽', '妈'),
    ('爺', '爷'), ('孫', '孙'), ('兒', '儿'), ('寶', '宝'), ('東', '东'), ('聽', '听'), ('動', '动'), ('無', '无'), ('張', '张'), ('熱', '热'),
    ('頂', '顶'), ('層', '层'), ('鄰', '邻'), ('術', '术'), ('檢', '检'), ('試', '试'), ('問', '问'), ('間', '间'), ('鎖', '锁'), ('綁', '绑'),
    ('騙', '骗'), ('輛', '辆'), ('騎', '骑'), ('託', '托'), ('幫', '帮'), ('擔', '担'), ('驚', '惊'), ('嚇', '吓'), ('亂', '乱'), ('聲', '声'),
    ('飛', '飞'), ('場', '场'), ('館', '馆'), ('飯', '饭'), ('餓', '饿'), ('樂', '乐'), ('歡', '欢'), ('禮', '礼'), ('週', '周'), ('師', '师'),
    ('擺', '摆'), ('屬', '属'), ('廚', '厨'), ('載', '载'), ('費', '费'), ('買', '买'), ('賣', '卖'), ('錯', '错'), ('鐵', '铁'), ('殺', '杀'),
    ('槍', '枪'), ('戰', '战'), ('獄', '狱'), ('違', '违'), ('規', '规'), ('視', '视'), ('劇', '剧'), ('書', '书'), ('寫', '写'), ('讀', '读'),
    ('紙', '纸'), ('畫', '画'), ('戲', '戏'), ('訊', '讯'), ('響', '响'), ('準', '准'), ('備', '备'), ('隨', '随'), ('題', '题'),
    ('撐', '撑'), ('鬧', '闹'), ('壞', '坏'), ('換', '换'), ('夠', '够'), ('嬰', '婴'), ('憂', '忧'), ('鐘', '钟'), ('島', '岛'), ('傳', '传'),
    ('別', '别'), ('撥', '拨'), ('鑰', '钥'), ('頁', '页'), ('較', '较'),
];

fn t2s(c: char) -> char {
    T2S.iter().find(|(t, _)| *t == c).map_or(c, |(_, s)| *s)
}

/// Latin letters without accents, full-width ASCII as ASCII, simplified Chinese;
/// anything that is not a letter or digit becomes a space.
fn fold_char(c: char, out: &mut String) {
    let c = match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        '\u{3000}' => ' ',
        _ => c,
    };
    for c in c.to_lowercase() {
        let base = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => "a",
            'ç' => "c",
            'è' | 'é' | 'ê' | 'ë' => "e",
            'ì' | 'í' | 'î' | 'ï' => "i",
            'ñ' => "n",
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => "o",
            'ù' | 'ú' | 'û' | 'ü' => "u",
            'ý' | 'ÿ' => "y",
            'ß' => "ss",
            'œ' => "oe",
            _ => {
                let c = t2s(c);
                out.push(if c.is_alphanumeric() { c } else { ' ' });
                continue;
            }
        };
        out.push_str(base);
    }
}

/// The folded text with single spaces between words and one at both ends (so
/// " word " matches whole words).
pub(crate) fn fold_spaced(s: &str) -> String {
    let mut raw = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        fold_char(c, &mut raw);
    }
    let mut out = String::with_capacity(raw.len() + 2);
    out.push(' ');
    for w in raw.split_whitespace() {
        out.push_str(w);
        out.push(' ');
    }
    out
}

/// The folded text without any space: "ne raccroche" and "neraccroche" read the same.
pub(crate) fn fold_compact(s: &str) -> String {
    fold_spaced(s).chars().filter(|c| !c.is_whitespace()).collect()
}

fn any(cues: &[Cue], spaced: &str, compact: &str) -> bool {
    cues.iter().any(|c| if c.compact { compact.contains(&c.pattern) } else { spaced.contains(&c.pattern) })
}

/// One sentence. The cues of every language are read together (a drill in
/// Spanish, "simulacro", must not leave Portuguese "incendio" standing); only an
/// ask and its "at once" have to come from the same language.
fn sentence_urgent(spaced: &str, compact: &str) -> bool {
    let all = compiled();
    let has = |pick: fn(&Compiled) -> &Vec<Cue>| all.iter().any(|c| any(pick(c), spaced, compact));
    if has(|c| &c.calm) {
        return false;
    }
    if has(|c| &c.urge) || all.iter().any(|c| any(&c.ask, spaced, compact) && any(&c.at_once, spaced, compact)) {
        return true;
    }
    if !has(|c| &c.events) || has(|c| &c.fine) {
        return false;
    }
    !has(|c| &c.past) || has(|c| &c.now)
}

/// The sentences of what was said, each judged alone ("last week" in one does not
/// cancel an emergency in the next).
fn sentences(text: &str) -> impl Iterator<Item = &str> {
    text.split(['.', '!', '?', '。', '！', '？', '\n', '…', ';', '；'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Whether the caller's words name a real emergency (see the module text).
pub fn is_urgent(text: &str) -> bool {
    sentences(text).any(|s| {
        let spaced = fold_spaced(s);
        let compact: String = spaced.chars().filter(|c| !c.is_whitespace()).collect();
        sentence_urgent(&spaced, &compact)
    })
}

/// The language the caller speaks in their recent words, when it can be told: by
/// script for Chinese, Japanese and Korean, by common words and letters for the
/// others.
pub fn caller_language(said: &str) -> Option<Lang> {
    let tail: String = {
        let chars: Vec<char> = said.chars().collect();
        chars[chars.len().saturating_sub(300)..].iter().collect()
    };
    let (mut hangul, mut kana, mut han, mut trad, mut simp) = (0, 0, 0, 0, 0);
    for c in tail.chars() {
        match c as u32 {
            0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => hangul += 1,
            0x3040..=0x30FF => kana += 1,
            0x4E00..=0x9FFF => {
                han += 1;
                if T2S.iter().any(|(t, _)| *t == c) {
                    trad += 1;
                } else if !matches!(c, '里' | '周' | '后') && T2S.iter().any(|(_, s)| *s == c) {
                    simp += 1;
                }
            }
            _ => {}
        }
    }
    if hangul >= 2 {
        return Some(Lang::Ko);
    }
    if kana >= 2 {
        return Some(Lang::Ja);
    }
    if han >= 2 {
        return Some(if simp > trad { Lang::ZhCn } else { Lang::ZhTw });
    }
    latin_language(&tail)
}

fn latin_language(text: &str) -> Option<Lang> {
    const WORDS: [(Lang, &[&str]); 5] = [
        (Lang::En, &["the", "and", "is", "are", "was", "i", "i'm", "you", "he", "she", "it", "to", "my", "his", "her", "in", "on", "at", "of", "just", "there", "has", "been", "have", "need", "we", "our", "someone", "with", "for", "they're", "it's", "get", "back", "call", "whenever", "so", "what", "this", "that", "your", "can", "will"]),
        (Lang::Es, &["el", "la", "los", "las", "y", "en", "se", "mi", "me", "es", "está", "por", "una", "un", "lo", "le", "ya", "hay", "del", "al", "muy", "con", "no", "estoy", "nos"]),
        (Lang::Fr, &["le", "la", "les", "est", "et", "je", "il", "elle", "on", "du", "des", "une", "un", "pas", "dans", "à", "au", "aux", "s'est", "c'est", "j'ai", "mon", "ma", "ne", "qu'il", "depuis", "suis"]),
        (Lang::De, &["der", "die", "das", "ist", "ich", "und", "nicht", "er", "sie", "es", "ein", "eine", "einen", "im", "zu", "mit", "hat", "wir", "mein", "meine", "bei", "uns", "sich", "sofort", "schon", "vom", "seit", "wurde", "wurden", "hab"]),
        (Lang::Pt, &["o", "os", "as", "e", "em", "do", "da", "no", "na", "meu", "minha", "um", "uma", "não", "tá", "tô", "pra", "pro", "com", "ele", "ela", "já", "foi", "é", "eu", "tão", "ao", "hoje", "teve", "você", "gente", "isso", "bora", "ontem", "dela", "dele"]),
    ];
    let lower = text.to_lowercase().replace(['\u{2019}', '\u{2018}', '\u{60}'], "'");
    let mut score = [0i32; 5];
    for token in lower.split(|c: char| !(c.is_alphabetic() || c == '\'')).filter(|t| !t.is_empty()) {
        for (i, (_, words)) in WORDS.iter().enumerate() {
            if words.contains(&token) {
                score[i] += 1;
            }
        }
        if ["l'", "d'", "qu'", "j'", "s'", "c'", "m'", "n'"].iter().any(|p| token.starts_with(p) && token.len() > p.len()) {
            score[2] += 1;
        }
    }
    for c in lower.chars() {
        match c {
            'ñ' | '¿' | '¡' => score[1] += 2,
            'ã' | 'õ' => score[4] += 2,
            'ç' => {
                score[2] += 1;
                score[4] += 1;
            }
            'ß' | 'ä' | 'ö' | 'ü' => score[3] += 2,
            'è' | 'ê' | 'à' | 'ù' | 'î' | 'ô' | 'û' | 'œ' | 'ë' => score[2] += 1,
            'á' | 'í' | 'ó' | 'ú' => {
                score[1] += 1;
                score[4] += 1;
            }
            _ => {}
        }
    }
    let (best, top) = score.iter().enumerate().max_by_key(|(_, s)| **s)?;
    (*top > 0).then_some(WORDS[best].0)
}

/// The language's name for an instruction to the model.
pub fn language_name(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw | Lang::ZhCn => "Chinese",
        Lang::En => "English",
        Lang::Ja => "Japanese",
        Lang::Ko => "Korean",
        Lang::Es => "Spanish",
        Lang::Fr => "French",
        Lang::De => "German",
        Lang::Pt => "Portuguese",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANGS: [&str; 9] = ["zh-TW", "zh-CN", "en", "ja", "ko", "es", "fr", "de", "pt"];

    /// The emergencies every language is tested with.
    pub(crate) const SITUATIONS: [&str; 11] = [
        "family member collapsed / taken to the ER",
        "car crash",
        "fire / gas leak / flood",
        "child or elder missing",
        "someone just died / is dying",
        "robbery / break-in, police called",
        "trapped (elevator, mountain)",
        "stranded abroad, passport and wallet gone",
        "something bad at home, come back now",
        "work emergency (system down)",
        "school called, child hurt",
    ];

    /// The mentions every language is tested with: none of them is urgent.
    pub(crate) const MENTIONS: [&str; 8] = [
        "past check-up, nothing serious",
        "fire drill",
        "accident on the news",
        "a death last year",
        "busy at work, everything a rush",
        "joking about starving",
        "not urgent, call back later",
        "bare word (surprise, right now, tough, time flies)",
    ];

    /// (language, situation, what a caller would say)
    pub(crate) const URGENT: &[(&str, usize, &str)] = &[
        ("zh-TW", 0, "我媽剛剛在家裡昏倒，救護車已經載去急診了，你快來！"),
        ("zh-TW", 1, "阿傑出車禍了，人還卡在車裡，你快點過來！"),
        ("zh-TW", 2, "家裡廚房失火了，消防車已經到了！"),
        ("zh-TW", 3, "小寶在夜市走失了，我們到處都找不到他！"),
        ("zh-TW", 4, "外婆剛剛過世了，醫院要家屬馬上過去。"),
        ("zh-TW", 5, "我剛剛被搶了，包包手機都被拿走，已經報警了。"),
        ("zh-TW", 6, "我被困在電梯裡快一個小時了，怎麼按都出不去！"),
        ("zh-TW", 7, "我在大阪，護照跟錢包全被偷了，身上一毛錢都沒有。"),
        ("zh-TW", 8, "家裡出大事了，叫他現在馬上回來！"),
        ("zh-TW", 9, "客戶的系統整個掛了，線上全部停擺，要他立刻處理。"),
        ("zh-TW", 10, "學校老師打來說，弟弟在操場摔傷了頭，現在要送醫。"),
        ("zh-CN", 0, "我爸刚才在家晕倒了，已经叫了救护车送急诊，你赶紧过来！"),
        ("zh-CN", 1, "你弟出车祸了，人现在在医院抢救。"),
        ("zh-CN", 2, "家里煤气泄漏了，我们都跑到楼下了，你快回来！"),
        ("zh-CN", 3, "妈妈在超市走丢了，到现在都没找到！"),
        ("zh-CN", 4, "姥姥快不行了，医生让家属赶紧过去。"),
        ("zh-CN", 5, "家里被小偷撬门进去了，我已经报警了。"),
        ("zh-CN", 6, "我们在山上迷路了，天快黑了，手机也快没电了。"),
        ("zh-CN", 7, "我在机场，护照和钱包都丢了，回不了国。"),
        ("zh-CN", 8, "家里出大事了，让他马上回来一趟！"),
        ("zh-CN", 9, "线上服务器宕机了，客户那边已经炸锅了，得马上找他。"),
        ("zh-CN", 10, "学校老师打电话来，说孩子在体育课上摔伤了，正往医院送。"),
        ("en", 0, "Mom collapsed at home and the ambulance is taking her to the ER right now."),
        ("en", 1, "Jake was in a car accident, they're still cutting him out of the car."),
        ("en", 2, "The kitchen's on fire, the fire department is on the way!"),
        ("en", 3, "We can't find Lily anywhere, she wandered off at the mall."),
        ("en", 4, "Grandpa just passed away, the hospice says the family should come now."),
        ("en", 5, "Someone broke into the house and I've already called the police."),
        ("en", 6, "I'm stuck in an elevator and it's been almost an hour."),
        ("en", 7, "I'm in Madrid and my passport and wallet got stolen, I have no money and no way home."),
        ("en", 8, "Something really bad happened at home, tell him to come home right now."),
        ("en", 9, "Our client's whole system is down and production has stopped, we need him on this immediately."),
        ("en", 10, "The school just called, Ben fell off the jungle gym and hit his head."),
        ("ja", 0, "母が家で倒れて、今救急車で病院に運ばれてます。すぐ来てください。"),
        ("ja", 1, "兄が車の事故に遭って、今手術中です。"),
        ("ja", 2, "家が火事なんです！消防車がもう来てます！"),
        ("ja", 3, "おばあちゃんが散歩から帰ってこなくて、どこにもいないんです。"),
        ("ja", 4, "祖父がさっき亡くなりました。家族はすぐ病院に来てくださいって言われてます。"),
        ("ja", 5, "空き巣に入られて、さっき警察を呼びました。"),
        ("ja", 6, "エレベーターに閉じ込められて、もう一時間近く経つんです。"),
        ("ja", 7, "旅行先でパスポートも財布も盗まれて、帰る手段がないんです。"),
        ("ja", 8, "実家で大変なことが起きたので、今すぐ帰ってくるように伝えてください。"),
        ("ja", 9, "取引先のシステムが全部落ちてて、至急対応してほしいんです。"),
        ("ja", 10, "学校から電話があって、息子が体育の時間にケガをしたそうです。"),
        ("ko", 0, "엄마가 집에서 쓰러지셔서 지금 구급차로 응급실 가고 있어요. 빨리 와 주세요."),
        ("ko", 1, "형이 교통사고가 나서 지금 수술 중이에요."),
        ("ko", 2, "집에 불이 났어요! 소방차 오고 있어요!"),
        ("ko", 3, "아버지가 산책 나가셨는데 연락도 안 되고 아직 안 들어오세요."),
        ("ko", 4, "할머니가 방금 돌아가셨어요. 가족들 다 병원으로 오래요."),
        ("ko", 5, "집에 도둑이 들어서 방금 경찰에 신고했어요."),
        ("ko", 6, "엘리베이터에 갇혀서 한 시간째 못 나가고 있어요."),
        ("ko", 7, "여행 중에 여권이랑 지갑을 다 잃어버려서 돌아갈 방법이 없어요."),
        ("ko", 8, "집에 큰일이 났어요. 지금 당장 들어오라고 전해 주세요."),
        ("ko", 9, "거래처 서버가 다운돼서 서비스가 전부 멈췄어요. 바로 처리해 주셔야 해요."),
        ("ko", 10, "학교에서 전화 왔는데, 아들이 체육 시간에 다쳐서 보건실에 있대요."),
        ("es", 0, "Mi madre se desmayó en casa y la llevan en ambulancia a urgencias, que venga ya."),
        ("es", 1, "Mi hermano tuvo un accidente de coche y lo están operando."),
        ("es", 2, "¡Se está quemando la cocina! Ya vienen los bomberos."),
        ("es", 3, "No encontramos al niño por ningún lado, se perdió en el centro comercial."),
        ("es", 4, "Mi abuelo acaba de fallecer, piden que venga toda la familia."),
        ("es", 5, "Nos robaron en casa y ya llamamos a la policía."),
        ("es", 6, "Me quedé atrapada en el ascensor, llevo casi una hora aquí."),
        ("es", 7, "Estoy en Roma, me robaron el pasaporte y la cartera y no tengo cómo volver."),
        ("es", 8, "Pasó algo muy grave en casa, dile que se venga ahora mismo."),
        ("es", 9, "Se cayó todo el sistema del cliente y la producción está parada, necesitamos que lo vea ya."),
        ("es", 10, "Llamaron del colegio: la niña se cayó en el recreo y se rompió el brazo."),
        ("fr", 0, "Maman a fait un malaise, les pompiers l'emmènent aux urgences, il faut qu'il vienne."),
        ("fr", 1, "Mon frère a eu un accident de voiture, il est au bloc."),
        ("fr", 2, "Il y a le feu dans la cuisine, les pompiers arrivent !"),
        ("fr", 3, "On ne retrouve plus le petit, il s'est perdu au centre commercial."),
        ("fr", 4, "Papy vient de mourir, l'hôpital demande que la famille vienne tout de suite."),
        ("fr", 5, "On s'est fait cambrioler, j'ai déjà appelé la police."),
        ("fr", 6, "Je suis coincée dans l'ascenseur depuis presque une heure."),
        ("fr", 7, "Je suis à Lisbonne, on m'a volé mon passeport et mon portefeuille, je ne peux pas rentrer."),
        ("fr", 8, "Il s'est passé quelque chose de grave à la maison, dis-lui de rentrer tout de suite."),
        ("fr", 9, "Le serveur du client est tombé, toute la prod est à l'arrêt, il faut qu'il s'en occupe tout de suite."),
        ("fr", 10, "L'école a appelé, la petite est tombée dans la cour et s'est cassé le bras."),
        ("de", 0, "Mama ist zu Hause umgekippt, der Rettungswagen bringt sie gerade in die Notaufnahme."),
        ("de", 1, "Mein Bruder hatte einen Autounfall, er wird gerade operiert."),
        ("de", 2, "In der Küche brennt es, die Feuerwehr ist unterwegs!"),
        ("de", 3, "Opa ist seit heute Morgen verschwunden, wir finden ihn nirgends."),
        ("de", 4, "Opa ist gerade gestorben, das Hospiz sagt, die Familie soll sofort kommen."),
        ("de", 5, "Bei uns wurde eingebrochen, ich hab schon die Polizei gerufen."),
        ("de", 6, "Ich stecke seit fast einer Stunde im Aufzug fest."),
        ("de", 7, "Ich bin in Rom, Pass und Geldbeutel wurden geklaut, ich komm nicht mehr nach Hause."),
        ("de", 8, "Zu Hause ist was Schlimmes passiert, sag ihm, er soll sofort heimkommen."),
        ("de", 9, "Beim Kunden ist das ganze System ausgefallen, die Produktion steht, er muss sich sofort drum kümmern."),
        ("de", 10, "Die Schule hat angerufen, der Kleine ist vom Klettergerüst gefallen und hat sich am Kopf verletzt."),
        ("pt", 0, "Minha mãe desmaiou em casa e a ambulância tá levando ela pro pronto-socorro."),
        ("pt", 1, "Meu irmão sofreu um acidente de carro e tá em cirurgia."),
        ("pt", 2, "A cozinha tá pegando fogo, os bombeiros já tão vindo!"),
        ("pt", 3, "A gente não encontra o Pedrinho em lugar nenhum, ele sumiu no shopping."),
        ("pt", 4, "Meu avô acabou de falecer, pediram pra família ir agora."),
        ("pt", 5, "Assaltaram a gente em casa e eu já chamei a polícia."),
        ("pt", 6, "Tô presa no elevador faz quase uma hora."),
        ("pt", 7, "Tô em Paris, roubaram meu passaporte e minha carteira, não tenho como voltar."),
        ("pt", 8, "Aconteceu uma coisa séria em casa, fala pra ele vir agora mesmo."),
        ("pt", 9, "O sistema do cliente caiu inteiro e a produção parou, ele precisa ver isso agora."),
        ("pt", 10, "A escola ligou, a menina caiu no recreio e quebrou o braço."),
    ];

    /// (language, mention, what a caller would say): none is urgent.
    pub(crate) const NOT_URGENT: &[(&str, usize, &str)] = &[
        ("zh-TW", 0, "我上禮拜去醫院拿藥，沒什麼事啦。"),
        ("zh-TW", 1, "公司上次辦消防演習，大家都跑到頂樓去。"),
        ("zh-TW", 2, "新聞說高速公路上有車禍，你回家路上小心喔。"),
        ("zh-TW", 3, "我奶奶去年過世了，今天剛好是她的忌日。"),
        ("zh-TW", 4, "最近工作好忙，每件事都好急喔。"),
        ("zh-TW", 5, "急死了，我餓到快不行了，晚上要不要一起吃飯？"),
        ("zh-TW", 6, "不急啦，你有空再回我就好。"),
        ("zh-TW", 7, "好意外你還記得我。"),
        ("zh-CN", 0, "我上周去医院开了点药，没啥事。"),
        ("zh-CN", 1, "上次单位搞消防演练，大家都下楼了。"),
        ("zh-CN", 2, "新闻上说高速上出了车祸，你开车慢点。"),
        ("zh-CN", 3, "我奶奶去年走的，今天是她的忌日。"),
        ("zh-CN", 4, "这阵子工作忙死了，天天赶进度。"),
        ("zh-CN", 5, "急死我了，饿得不行，晚上一起吃饭呗？"),
        ("zh-CN", 6, "不着急，你有空再回我就行。"),
        ("zh-CN", 7, "真没想到你还记得我，太意外了。"),
        ("en", 0, "I went to the hospital last week to pick up my meds, nothing serious."),
        ("en", 1, "We had a fire drill at work this morning, total chaos."),
        ("en", 2, "The news said there was a bad car crash on the highway, so drive safe."),
        ("en", 3, "My grandma passed away last year, so today's kind of a tough day."),
        ("en", 4, "Work's been crazy lately, everything's a rush."),
        ("en", 5, "I'm literally dying of hunger, wanna grab dinner tonight?"),
        ("en", 6, "No rush, just call me back whenever you get a chance."),
        ("en", 7, "Right now I'm just calling to say hi."),
        ("ja", 0, "先週、病院で薬をもらいましたが、大丈夫です。"),
        ("ja", 1, "この前会社で避難訓練があって、階段で外まで出たよ。"),
        ("ja", 2, "ニュースで高速で事故があったって言ってたから、運転気をつけてね。"),
        ("ja", 3, "祖母は去年亡くなったんだけど、今日が命日なんだ。"),
        ("ja", 4, "最近仕事がバタバタしてて、何もかも急ぎなんだよね。"),
        ("ja", 5, "お腹すいて死にそう！今夜ご飯行かない？"),
        ("ja", 6, "急ぎじゃないから、時間あるときに折り返してね。"),
        ("ja", 7, "最近は仕事が大変ですね。"),
        ("ko", 0, "지난주에 병원에서 약을 받았는데 괜찮아요."),
        ("ko", 1, "저번에 회사에서 소방 훈련했는데 다 같이 계단으로 내려갔어요."),
        ("ko", 2, "뉴스 보니까 고속도로에서 교통사고 났다더라, 운전 조심해."),
        ("ko", 3, "할머니가 작년에 돌아가셨는데, 오늘이 기일이에요."),
        ("ko", 4, "요즘 일이 너무 바빠서 다 급하게 처리하고 있어요."),
        ("ko", 5, "배고파 죽겠다, 오늘 저녁에 밥 먹을래?"),
        ("ko", 6, "급한 거 아니니까 시간 날 때 연락 줘."),
        ("ko", 7, "시간이 빨리 가네요, 벌써 가을이에요."),
        ("es", 0, "La semana pasada fui al hospital por mis medicinas, nada grave."),
        ("es", 1, "Hoy hubo simulacro de incendio en la oficina y todos salimos a la calle."),
        ("es", 2, "Dijeron en las noticias que hubo un choque en la autopista, maneja con cuidado."),
        ("es", 3, "Mi abuela falleció el año pasado, hoy es su aniversario."),
        ("es", 4, "Ando a mil en el trabajo, todo es para ayer."),
        ("es", 5, "¡Me muero de hambre! ¿Cenamos esta noche?"),
        ("es", 6, "No es urgente, llámame cuando puedas."),
        ("es", 7, "Ahora mismo estoy en el súper, te llamo luego."),
        ("fr", 0, "La semaine dernière je suis allé à l'hôpital chercher mes médicaments, rien de grave."),
        ("fr", 1, "On a eu un exercice incendie au boulot ce matin, tout le monde dehors."),
        ("fr", 2, "Ils ont dit aux infos qu'il y a eu un accident sur l'autoroute, fais attention."),
        ("fr", 3, "Ma grand-mère est décédée l'année dernière, aujourd'hui ça aurait été son anniversaire."),
        ("fr", 4, "En ce moment au boulot tout est urgent, je suis débordé."),
        ("fr", 5, "Je meurs de faim, on dîne ensemble ce soir ?"),
        ("fr", 6, "Rien d'urgent, rappelle-moi quand tu peux."),
        ("fr", 7, "Là tout de suite je suis au supermarché, je te rappelle."),
        ("de", 0, "Letzte Woche war ich im Krankenhaus wegen meiner Tabletten, nichts Schlimmes."),
        ("de", 1, "Heute Morgen hatten wir eine Feueralarm-Übung im Büro, alle mussten raus."),
        ("de", 2, "In den Nachrichten kam, dass es auf der Autobahn einen schweren Unfall gab, fahr vorsichtig."),
        ("de", 3, "Meine Oma ist letztes Jahr gestorben, heute wäre ihr Geburtstag gewesen."),
        ("de", 4, "Bei der Arbeit ist gerade alles dringend, ich komm kaum hinterher."),
        ("de", 5, "Ich sterbe vor Hunger, gehen wir heute Abend essen?"),
        ("de", 6, "Ist nicht dringend, ruf einfach zurück, wenn du Zeit hast."),
        ("de", 7, "Ich bin gerade im Supermarkt, ich ruf dich gleich zurück."),
        ("pt", 0, "Semana passada fui ao hospital buscar remédio, nada grave."),
        ("pt", 1, "Hoje teve simulado de incêndio no escritório, todo mundo desceu de escada."),
        ("pt", 2, "Deu no jornal que teve um acidente feio na estrada, vai com cuidado."),
        ("pt", 3, "Minha avó faleceu ano passado, hoje é aniversário dela."),
        ("pt", 4, "No trampo tá tudo pra ontem, tudo urgente."),
        ("pt", 5, "Tô morrendo de fome, bora jantar hoje?"),
        ("pt", 6, "Não é urgente, me liga quando puder."),
        ("pt", 7, "Agora mesmo tô no mercado, te ligo depois."),
    ];

    #[test]
    fn every_language_has_every_situation_and_every_mention() {
        for lang in LANGS {
            for (i, s) in SITUATIONS.iter().enumerate() {
                assert_eq!(URGENT.iter().filter(|(l, k, _)| *l == lang && *k == i).count(), 1, "{lang}: {s}");
            }
            for (i, m) in MENTIONS.iter().enumerate() {
                assert_eq!(NOT_URGENT.iter().filter(|(l, k, _)| *l == lang && *k == i).count(), 1, "{lang}: {m}");
            }
        }
        assert_eq!(URGENT.len(), LANGS.len() * SITUATIONS.len());
        assert_eq!(NOT_URGENT.len(), LANGS.len() * MENTIONS.len());
    }

    #[test]
    fn real_emergencies_are_urgent_in_every_language() {
        let missed: Vec<String> = URGENT
            .iter()
            .filter(|(_, _, s)| !is_urgent(s))
            .map(|(l, k, s)| format!("{l} [{}]: {s}", SITUATIONS[*k]))
            .collect();
        assert!(missed.is_empty(), "missed:\n{}", missed.join("\n"));
    }

    #[test]
    fn mentions_without_an_emergency_are_not_urgent_in_every_language() {
        let wrong: Vec<String> = NOT_URGENT
            .iter()
            .filter(|(_, _, s)| is_urgent(s))
            .map(|(l, k, s)| format!("{l} [{}]: {s}", MENTIONS[*k]))
            .collect();
        assert!(wrong.is_empty(), "taken as urgent:\n{}", wrong.join("\n"));
    }

    #[test]
    fn a_past_mention_in_one_sentence_does_not_cancel_an_emergency_in_the_next() {
        assert!(is_urgent("我上週才去醫院拿藥。結果我爸剛剛又昏倒，送急診了！"));
        assert!(is_urgent("I saw him last week. Now Dad collapsed and the ambulance is here!"));
        assert!(!is_urgent("我奶奶去年過世了。今天只是想跟你聊聊。"));
    }

    #[test]
    fn the_callers_language_is_told_from_what_they_said() {
        let wrong: Vec<String> = URGENT
            .iter()
            .chain(NOT_URGENT.iter().filter(|(_, k, _)| *k != 7))
            .filter_map(|(lang, _, s)| {
                let got = caller_language(s).map(|l| if l.is_zh() { "zh" } else { l.code() });
                let want = if lang.starts_with("zh") { "zh" } else { lang };
                (got != Some(want)).then(|| format!("{lang} got {got:?}: {s}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        assert_eq!(caller_language(""), None);
        assert_eq!(caller_language("123 !!"), None);
    }

    #[test]
    fn folding_reads_spaces_accents_and_widths_alike() {
        assert_eq!(fold_compact("Ne raccroche pas"), fold_compact("neraccroche pas"));
        assert_eq!(fold_compact("Não desliga"), "naodesliga");
        assert_eq!(fold_compact("請你先別走"), "请你先别走");
        assert_eq!(fold_compact("ＤＯＮ\u{2019}Ｔ HANG UP"), "donthangup");
        assert_eq!(fold_spaced("Don't hang-up!"), " don t hang up ");
    }
}
