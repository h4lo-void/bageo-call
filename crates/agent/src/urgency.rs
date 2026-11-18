// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

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
        "失火", "火灾", "着火", "烧起来", "瓦斯外泄", "瓦斯漏", "瓦斯味", "煤气泄漏", "煤气味", "燃气泄漏", "燃气味", "闻到瓦斯", "消防队", "消防车", "消防员",
        "淹水", "淹大水", "水淹", "爆炸", "浓烟",
        "走失", "走丢", "人不见了", "孩子不见了", "小孩不见了", "失踪", "找不到他", "找不到她", "找不到人", "找不到孩子", "找不到小孩", "一直没回家", "到现在还没回家",
        "妈妈不见了", "爸爸不见了", "奶奶不见了", "爷爷不见了", "外婆不见了", "外公不见了", "他不见了", "她不见了",
        "过世", "去世", "往生", "病危", "快不行了", "没有心跳", "断气",
        "被抢", "抢劫", "遭小偷", "被偷了", "闯空门", "有人闯进", "撬门", "报警了", "已经报警", "警察来了", "被打了", "绑架",
        "被困", "困在", "卡在电梯", "关在电梯", "受困", "迷路了", "下不了山",
        "护照丢", "护照不见", "护照被偷", "钱包丢", "钱包被偷", "都丢了", "全丢了", "回不了国", "回不了家", "被扣留",
        "家里出事", "宕机", "当机", "系统挂了", "整个挂了", "全挂了", "系统崩溃", "系统当", "服务器挂", "伺服器挂", "伺服器当", "网站挂了", "停摆", "瘫痪", "炸锅",
        "工安意外", "工安事故", "工地出事", "工地意外", "工厂出事", "工厂意外", "工伤",
        "学校打来",
    ],
    urge: &[
        "紧急", "有急事", "急事", "十万火急", "救命", "快来", "快过来", "快点过来", "赶快过来", "赶紧过来", "快回来", "赶快回来",
        "赶紧回来", "你马上回", "你马上来", "你快点来", "现在需要你", "马上需要你", "需要你马上", "需要你立刻",
        "叫他马上", "要他马上", "请他马上", "让他马上", "叫他立刻", "请他立刻", "马上找他", "立刻找他", "马上叫他",
        "马上接电话", "赶快接电话", "快点接", "快接电话", "马上联络他", "找本人",
    ],
    ask: &[],
    at_once: &[],
    calm: &[
        "不急", "不用急", "不着急", "不用着急", "不赶", "没急事", "没有急事", "没什么急事", "有空再", "有空的时候", "慢慢来",
        "哈哈", "笑死", "开玩笑", "饿死", "饿到", "饿得", "新闻", "报导", "报道", "演习", "演练", "电影", "电视剧",
    ],
    fine: &["没事", "没什么事", "没啥事", "没什么大碍", "不严重", "已经好了", "已经出院", "出院了", "没出事", "还好没", "挺好的", "撑得住"],
    past: &[
        "昨天", "昨日", "前天", "上周", "上礼拜", "上星期", "上个月", "上次", "上一回", "去年", "前年", "以前", "之前", "小时候", "那时候",
        "几年前", "前几年", "一年前", "一年了", "一周年", "周年", "几个月前", "忌日",
    ],
    now: &["刚刚", "刚才", "刚被", "刚送", "刚出", "马上", "立刻"],
};

const JA: Cues = Cues {
    events: &[
        "倒れて", "倒れた", "倒れま", "救急車", "救急搬送", "搬送され", "病院に運ばれ", "意識がない", "意識不明", "脳卒中", "心筋梗塞", "心臓発作",
        "危篤", "集中治療室", "事故に遭", "事故にあ", "事故った", "交通事故", "はねられ", "ひかれ", "手術中", "緊急手術",
        "火事", "燃えて", "ガス漏れ", "ガスが漏れ", "ガス臭", "ガスの臭い", "消防車", "消防隊", "浸水", "水浸し", "爆発", "煙が",
        "行方不明", "迷子", "いなくなっ", "どこにもいない", "帰ってこな",
        "亡くなっ", "亡くなり", "死んだ", "もう長くない",
        "空き巣", "泥棒", "強盗", "ひったくり", "警察を呼", "警察に通報", "通報し",
        "閉じ込められ", "遭難", "道に迷", "取り残され", "盗まれ", "盗られ", "すられ", "スリに", "パスポート", "財布をなく", "財布を落と", "帰れな", "どうしようもな",
        "大変なこと", "システムが落ち", "サーバーが落ち", "サーバーダウン", "システム障害", "システムがクラッシュ", "サーバーがクラッシュ", "障害が発生", "全部止まっ",
        "労災", "現場で事故", "工場で事故", "作業中に事故",
        "学校から電話", "けがをし", "ケガをし", "怪我をし", "骨折",
    ],
    urge: &["緊急", "至急", "急ぎの用", "急用", "大至急", "すぐ繋いで", "すぐつないで", "すぐ来て", "すぐに来て", "今すぐ来て", "今すぐ必要", "今すぐ", "助けて"],
    ask: &[],
    at_once: &[],
    calm: &[
        "急ぎじゃない", "急ぎではない", "急がない", "急がなくて", "急ぎません", "緊急じゃない", "緊急ではない", "緊急ってわけじゃない", "緊急という", "緊急じゃなく",
        "時間あるとき", "時間がある時", "お手すきの", "暇なとき", "ゆっくりで",
        "ニュース", "訓練", "ドラマ", "映画", "冗談", "死にそう", "お腹すいて", "腹減",
    ],
    fine: &["大丈夫", "大したことない", "たいしたことない", "退院し", "もう退院", "なんとかなっ"],
    past: &["昨日", "きのう", "一昨日", "先週", "先月", "去年", "昨年", "一昨年", "前に", "昔", "この前", "以前", "命日", "一周忌", "一年前", "一年経", "1年経", "何年も前"],
    now: &["さっき", "たった今", "今すぐ", "今さっき"],
};

const KO: Cues = Cues {
    events: &[
        "쓰러지", "쓰러졌", "구급차", "응급실", "실려갔", "실려 갔", "의식이 없", "뇌졸중", "심장마비", "중환자실", "위독",
        "교통사고", "사고가 났", "사고 났", "사고를 당", "차에 치", "수술 중",
        "불이 났", "불났", "화재", "가스가 새", "가스 새", "가스 누출", "가스 냄새", "소방차", "소방대", "물에 잠겼", "침수", "폭발",
        "실종", "길을 잃", "없어졌", "안 들어오", "찾을 수가 없", "어디에도 없",
        "돌아가셨", "돌아가시", "가망이 없",
        "도둑이 들", "도둑맞", "강도가", "강도를", "강도 당", "소매치기", "경찰에 신고", "신고했", "경찰 불렀",
        "갇혀", "갇혔", "고립", "조난",
        "여권이랑 지갑", "여권을 잃", "지갑을 잃", "다 잃어버", "도난", "돌아갈 방법이 없", "돌아갈 수가 없",
        "큰일 났", "큰일이 났", "서버가 다운", "서버 다운", "시스템이 다운", "서버가 죽", "시스템이 죽", "시스템 장애", "장애가 발생", "전부 멈췄",
        "산업재해", "현장에서 사고", "공장에서 사고", "작업 중 사고",
        "학교에서 전화", "다쳐서", "다쳤", "골절",
    ],
    urge: &["긴급", "급한 일", "급한 용", "급합니다", "급해요", "응급", "빨리 바꿔", "빨리 와", "지금 당장 와", "지금 바로 필요", "당장", "살려 주세요", "살려줘"],
    ask: &[],
    at_once: &[],
    calm: &[
        "급하지 않", "안 급해", "급한 거 아니", "급한 건 아니", "긴급한 거 아니", "긴급한 건 아니", "긴급하지 않", "시간 날 때", "천천히", "편할 때",
        "뉴스", "훈련", "드라마", "영화", "농담", "배고파", "죽겠다",
    ],
    fine: &["괜찮아", "괜찮습니다", "별일 아니", "별거 아니", "퇴원했", "이미 퇴원", "잘 지내"],
    past: &["어제", "그저께", "지난주", "지난달", "작년", "재작년", "예전에", "저번에", "지난번", "기일", "1주기", "1년 됐", "일년 됐", "1년 전", "몇 년 전"],
    now: &["방금", "방금 전에", "당장"],
};

const EN: Cues = Cues {
    events: &[
        "collapsed", "passed out", "ambulance", "to the er", "in the er", "the emergency room", "taken to the hospital", "rushed to the hospital",
        "had a stroke", "heart attack", "unconscious", "not breathing", "icu", "on life support",
        "car accident", "car crash", "in an accident", "been an accident", "hit by a car", "got hit by",
        "on fire", "house is burning", "fire department", "fire trucks", "firefighters", "smell gas", "gas smell", "smells like gas", "gas leak", "flooding", "flooded", "explosion",
        "wandered off", "is missing", "went missing", "gone missing", "can't find her", "can't find him",
        "passed away", "just died", "has died", "is dying", "won't make it", "not going to make it",
        "broke into", "break in", "got robbed", "been robbed", "was robbed", "mugged", "called the police", "called the cops", "called 911", "calling 911",
        "stuck in an elevator", "stuck in the elevator", "trapped", "stranded", "lost on the",
        "got stolen", "was stolen", "were stolen", "lost my passport", "lost my wallet", "no way home",
        "something bad happened", "something terrible happened", "something happened at home",
        "system is down", "system went down", "system just crashed", "system crashed", "systems crashed", "server crashed", "servers crashed", "everything crashed",
        "servers are down", "server is down", "site is down", "production is down", "production is stopped", "production has stopped", "outage",
        "workplace accident", "accident at the site", "accident at the factory", "accident at the plant", "accident at work", "hurt at work", "injured at work",
        "school called", "school just called", "nurse called", "hit his head", "hit her head", "broke his arm", "broke her arm", "hurt at school",
    ],
    urge: &[
        "emergency", "it's urgent", "it is urgent", "this is urgent", "very urgent", "urgently", "something urgent", "urgent matter", "life or death",
        "need you right now", "need you immediately", "need you right away", "come right now", "come immediately", "get here right now",
    ],
    ask: &[
        "need him", "need to reach him", "need to talk to him", "need to speak to him", "get him", "put him on", "tell him to", "have him call", "find him", "reach him",
        "need you", "need to talk to you", "need to speak to you", "come home", "come here", "get here",
    ],
    at_once: &["right now", "right away", "immediately", "asap", "now"],
    calm: &[
        "not urgent", "not an emergency", "no emergency", "nothing urgent", "no rush", "no hurry", "whenever you", "when you get a chance", "no big deal",
        "just kidding", "kidding", "lol", "haha", "news", "drill", "fire alarm", "false alarm", "movie", "dying of hunger", "starving",
    ],
    fine: &[
        "nothing serious", "i'm fine", "all good", "doing fine", "doing ok", "doing okay", "managing okay", "managing ok", "i'm okay", "i'm ok",
        "already discharged", "discharged", "recovered",
    ],
    past: &[
        "yesterday", "last week", "last year", "last month", "years ago", "a year ago", "a year since", "been a year", "years since",
        "months ago", "a month ago", "a month since", "weeks ago", "a week ago", "days ago", "a while ago", "the other day", "anniversary",
    ],
    now: &[
        "just now", "just collapsed", "just died", "just passed", "just happened", "just had", "just got", "just been", "just called",
        "just taken", "just rushed", "just hit", "just broke", "just fell", "just crashed", "right now",
    ],
};

const ES: Cues = Cues {
    events: &[
        "se desmayo", "desmayada", "desmayado", "ambulancia", "a urgencias", "en urgencias", "ingresar al hospital", "acaba de ingresar",
        "esta ingresado", "esta ingresada", "lo ingresaron", "la ingresaron", "un derrame", "un infarto", "no respira", "inconsciente", "en la uci", "terapia intensiva",
        "accidente de coche", "accidente de carro", "accidente de trafico", "tuvo un accidente", "hubo un accidente", "atropello", "atropellado", "atropellada", "choque",
        "lo estan operando", "la estan operando",
        "se esta quemando", "se quema", "incendio", "bomberos", "fuga de gas", "huele a gas", "olor a gas", "se inundo", "inundacion", "explosion",
        "no encontramos", "no lo encontramos", "no la encontramos", "se perdio", "desaparecido", "desaparecida", "desaparecio",
        "fallecio", "acaba de fallecer", "murio", "se esta muriendo", "muy grave", "esta grave",
        "nos robaron", "me robaron", "me asaltaron", "asalto", "entraron a robar", "llamamos a la policia", "llame a la policia",
        "atrapado", "atrapada", "encerrado", "encerrada", "nos perdimos",
        "perdi el pasaporte", "perdi la cartera", "no tengo como volver",
        "algo muy grave", "algo grave", "se cayo todo", "se cayo el sistema", "se cayo el servidor", "colapso el sistema", "sistema caido", "servidor caido",
        "produccion parada", "produccion esta parada", "accidente laboral", "accidente en la fabrica", "accidente en la planta", "herido en el trabajo",
        "llamaron del colegio", "llamaron de la escuela", "se rompio", "se golpeo la cabeza", "se lastimo", "se hizo dano",
    ],
    urge: &["emergencia", "es urgente", "muy urgente", "urgentemente", "auxilio", "socorro", "te necesito ahora", "te necesito ya", "ven ahora mismo", "vente ya"],
    ask: &[
        "dile que", "digale que", "que venga", "que se venga", "comuniquelo", "pasamelo", "pasemelo", "que me llame", "necesito hablar con el",
        "te necesito", "necesito que vengas", "necesito hablar contigo", "ven a", "vente",
    ],
    at_once: &["ahora mismo", "ya", "enseguida", "inmediatamente", "urgente"],
    calm: &[
        "no es urgente", "no es una emergencia", "nada urgente", "no hay prisa", "sin prisa", "no corre prisa", "cuando puedas", "cuando tengas tiempo", "cuando pueda",
        "noticias", "simulacro", "alarma de incendio", "falsa alarma", "pelicula", "novela", "es broma", "jaja", "me muero de hambre",
    ],
    fine: &["nada grave", "no es nada", "estoy bien", "ya esta bien", "ya salio", "le dieron el alta", "lo llevo bien"],
    past: &["ayer", "anteayer", "la semana pasada", "el ano pasado", "el mes pasado", "hace anos", "hace un ano", "hace meses", "ya paso un ano", "el otro dia", "aniversario"],
    now: &["acaba de", "acabo de", "ahora mismo", "recien"],
};

const FR: Cues = Cues {
    events: &[
        "malaise", "aux urgences", "pompiers", "samu", "emmene a l hopital", "emmenee a l hopital", "l emmenent", "hospitalise", "hospitalisee",
        "avc", "crise cardiaque", "inconscient", "inconsciente", "respire plus", "reanimation",
        "accident de voiture", "eu un accident", "il y a eu un accident", "grave accident", "accident grave", "renverse par", "renversee par", "au bloc",
        "il y a le feu", "au feu", "pris feu", "prend feu", "incendie", "fuite de gaz", "sent le gaz", "odeur de gaz", "inonde", "inondee", "inondation", "degat des eaux", "explosion",
        "retrouve plus", "retrouve pas", "s est perdu", "s est perdue", "a disparu", "porte disparu",
        "vient de mourir", "est mort", "est morte", "decede", "decedee", "en train de mourir", "s en sortir",
        "cambriol*", "braque", "braques", "agresse", "agressee", "appele la police", "appele les flics",
        "coince*", "bloque dans", "bloquee dans", "perdus en montagne", "enferme*",
        "vole mon passeport", "vole mon portefeuille", "perdu mon passeport", "perdu mon portefeuille", "peux pas rentrer", "peux plus rentrer",
        "quelque chose de grave", "a l arret", "serveur en panne", "systeme en panne", "serveur est tombe", "systeme est tombe", "systeme a plante", "tout a plante",
        "serveur est en panne", "systeme est en panne", "production en panne",
        "accident du travail", "accident a l usine", "accident au travail", "blesse a l usine", "blesse au travail",
        "l ecole a appele", "l ecole vient d appeler", "s est casse", "s est cogne", "s est blesse*",
        "tombe dans l escalier", "tombee dans l escalier", "tombe dans les escaliers", "tombee dans les escaliers", "tombe dans les pommes", "tombee dans les pommes",
    ],
    urge: &[
        "c est urgent", "tres urgent", "une urgence", "c est une urgence", "urgence", "au secours", "a l aide",
        "viens tout de suite", "venez tout de suite", "rentre tout de suite", "besoin de toi maintenant", "besoin de toi tout de suite",
    ],
    ask: &[
        "passez le moi", "passe le moi", "dis lui de", "dites lui de", "qu il rentre", "qu il vienne", "qu il me rappelle", "j ai besoin de lui",
        "que tu viennes", "que vous veniez", "tu dois venir", "besoin de toi", "besoin de vous", "viens", "venez",
    ],
    at_once: &["tout de suite", "immediatement", "maintenant", "vite", "d urgence"],
    calm: &[
        "pas urgent", "pas une urgence", "non urgent", "rien d urgent", "pas presse", "rien ne presse", "quand tu peux", "quand vous pouvez", "quand tu auras", "quand vous aurez",
        "infos", "journal", "exercice", "alarme incendie", "fausse alerte", "film", "je rigole", "je plaisante", "mdr", "meurs de faim",
    ],
    fine: &["rien de grave", "je vais bien", "il va bien", "elle va bien", "est sorti de l hopital", "est sortie de l hopital", "deja sorti", "je tiens le coup"],
    past: &["hier", "avant hier", "la semaine derniere", "l annee derniere", "l an dernier", "l an passe", "le mois dernier", "il y a des annees", "il y a un an", "ca fait un an", "depuis un an", "un an apres", "ans apres", "l autre jour", "anniversaire"],
    now: &["vient de", "viens de", "tout de suite", "a l instant"],
};

const DE: Cues = Cues {
    events: &[
        "umgekippt", "zusammengebrochen", "bewusstlos", "rettungswagen", "krankenwagen", "notaufnahme", "notarzt", "schlaganfall", "herzinfarkt",
        "atmet nicht", "intensivstation", "ins krankenhaus gebracht", "eingeliefert",
        "autounfall", "unfall gehabt", "hatte einen unfall", "einen unfall", "schwerer unfall", "angefahren", "wird operiert", "wird gerade operiert",
        "brennt", "feuer ausgebrochen", "feuer gefangen", "feuerwehr", "riecht nach gas", "gasgeruch", "gasleck", "uberschwemmt", "unter wasser", "explosion",
        "feuer im haus", "feuer in der", "feuer in meiner",
        "verschwunden", "vermisst", "hat sich verlaufen", "haben uns verlaufen", "nicht nach hause gekommen",
        "gestorben", "verstorben", "im sterben", "stirbt",
        "eingebrochen", "einbruch", "uberfallen", "ausgeraubt", "beklaut", "polizei gerufen", "polizei angerufen",
        "im aufzug fest", "im fahrstuhl fest", "stecke fest", "stecken fest", "sitze fest", "sitzen fest", "eingesperrt", "eingeschlossen",
        "geklaut", "gestohlen", "pass verloren", "geldbeutel verloren", "nicht mehr nach hause",
        "was schlimmes passiert", "etwas schlimmes passiert", "ist was passiert",
        "system ausgefallen", "system ist ausgefallen", "system abgesturzt", "system ist abgesturzt", "server ausgefallen", "server abgesturzt",
        "server ist down", "system ist down", "produktion steht", "arbeitsunfall", "betriebsunfall", "unfall in der fabrik", "unfall im werk", "bei der arbeit verletzt",
        "schule hat angerufen", "verletzt", "gebrochen",
    ],
    urge: &["notfall", "es ist dringend", "sehr dringend", "dringend sprechen", "dringend erreichen", "brauche dich sofort", "brauche dich jetzt", "komm sofort"],
    ask: &[
        "muss ihn", "muss mit ihm", "verbinden sie mich", "holen sie ihn", "sag ihm er soll", "sagen sie ihm er soll", "er soll", "er muss",
        "brauche dich", "muss dich", "du musst", "komm bitte", "kommen sie",
    ],
    at_once: &["sofort", "jetzt", "dringend"],
    calm: &[
        "nicht dringend", "kein notfall", "nichts dringendes", "keine eile", "eilt nicht", "wenn du zeit hast", "wenn sie zeit haben", "in ruhe",
        "nachrichten", "ubung", "probealarm", "feueralarm", "fehlalarm", "film", "serie", "spass", "sterbe vor hunger",
    ],
    fine: &["nichts schlimmes", "alles gut", "geht mir gut", "geht ihm gut", "geht ihr gut", "entlassen", "schon entlassen", "komme klar"],
    past: &["gestern", "vorgestern", "letzte woche", "letztes jahr", "letzten monat", "vor jahren", "vor einem jahr", "ein jahr her", "seit einem jahr", "vor monaten", "neulich", "geburtstag", "todestag", "jahrestag"],
    now: &["gerade", "soeben", "sofort"],
};

const PT: Cues = Cues {
    events: &[
        "desmaiou", "ambulancia", "pronto socorro", "levado para o hospital", "levada para o hospital", "levado pro hospital", "levada pro hospital",
        "internado", "internada", "avc", "infarto", "nao esta respirando", "inconsciente", "uti",
        "acidente de carro", "sofreu um acidente", "houve um acidente", "teve um acidente", "acidente grave", "grave acidente", "atropelado", "atropelada", "bateu o carro", "em cirurgia",
        "pegando fogo", "pegou fogo", "incendio", "bombeiros", "vazamento de gas", "cheiro de gas", "cheiro forte de gas", "alagou", "alagada", "enchente", "explosao",
        "sumiu", "desaparecido", "desaparecida", "se perdeu", "nao achamos", "nao encontramos", "nao encontra",
        "faleceu", "acabou de falecer", "morreu", "esta morrendo", "ta morrendo", "nao vai resistir",
        "assaltaram", "assalto", "fomos assaltados", "roubaram", "arrombaram", "chamei a policia", "chamamos a policia",
        "preso no elevador", "presa no elevador", "presos no elevador", "ficou preso", "ficou presa", "perdidos na",
        "perdi o passaporte", "perdi a carteira", "nao tenho como voltar",
        "coisa seria", "coisa grave", "algo grave", "caiu inteiro", "caiu tudo", "sistema caiu", "sistema travou", "servidor caiu", "producao parou", "fora do ar",
        "acidente de trabalho", "acidente na fabrica", "machucou no trabalho", "ferido no trabalho",
        "a escola ligou", "ligaram da escola", "quebrou o braco", "quebrou a perna", "se machucou", "bateu a cabeca",
    ],
    urge: &["emergencia", "e urgente", "muito urgente", "urgencia", "socorro", "preciso de voce agora", "preciso muito de voce", "vem agora", "venha agora"],
    ask: &[
        "fala pra ele", "diz pra ele", "diga a ele", "pede pra ele", "preciso falar com ele", "chama ele",
        "preciso de voce", "preciso falar com voce", "vem pra ca", "venha", "precisa vir",
    ],
    at_once: &["agora mesmo", "ja", "urgente", "imediatamente", "agora"],
    calm: &[
        "nao e urgente", "nao e uma emergencia", "nada urgente", "sem pressa", "nao tem pressa", "quando puder", "quando der",
        "jornal", "noticia", "simulado", "simulacao", "alarme de incendio", "alarme falso", "filme", "novela", "brincadeira", "kkk", "morrendo de fome",
    ],
    fine: &["nada grave", "nao foi nada", "to bem", "estou bem", "ta tudo bem", "teve alta", "recebeu alta", "ja teve alta", "levando bem"],
    past: &["ontem", "anteontem", "semana passada", "ano passado", "mes passado", "faz anos", "ha anos", "faz um ano", "ha um ano", "faz meses", "outro dia", "aniversario"],
    now: &["acabou de", "acabei de", "agora mesmo"],
};

const IT: Cues = Cues {
    events: &[
        "svenuto", "svenuta", "ha perso i sensi", "privo di sensi", "ambulanza", "pronto soccorso", "in ospedale", "ricoverato", "ricoverata",
        "ictus", "infarto", "non respira", "terapia intensiva", "rianimazione",
        "incidente stradale", "incidente d auto", "incidente in auto", "un incidente", "investito", "investita", "sala operatoria", "stanno operando",
        "a fuoco", "sta bruciando", "incendio", "vigili del fuoco", "pompieri", "fuga di gas", "odore di gas", "puzza di gas", "allagamento", "allagato", "allagata", "esplosione",
        "non lo troviamo", "non la troviamo", "si e perso", "si e persa", "scomparso", "scomparsa", "non e tornato", "non e tornata",
        "e morto", "e morta", "e deceduto", "e deceduta", "sta morendo", "in fin di vita", "non ce la fa",
        "ci hanno derubato", "mi hanno derubato", "rapina", "furto in casa", "entrati i ladri", "chiamato la polizia", "chiamato i carabinieri",
        "bloccato in ascensore", "bloccata in ascensore", "chiuso in ascensore", "chiusa in ascensore", "intrappolato", "intrappolata", "persi in montagna",
        "perso il passaporto", "perso il portafoglio", "rubato il passaporto", "rubato il portafoglio", "non ho come tornare",
        "qualcosa di grave", "sistema e fermo", "sistema fermo", "sistema bloccato", "server e caduto", "server bloccato", "produzione ferma", "produzione e ferma",
        "infortunio sul lavoro", "incidente in fabbrica", "incidente al lavoro", "ferito sul lavoro",
        "chiamato da scuola", "chiamato la scuola", "battuto la testa", "rotto il braccio", "rotto una gamba", "fatto male",
    ],
    urge: &["emergenza", "e urgente", "molto urgente", "urgentemente", "aiuto", "soccorso", "ho bisogno di te subito", "vieni subito", "venga subito", "torna subito"],
    ask: &["me lo passi", "digli di", "gli dica di", "fatti richiamare", "devo parlargli", "ho bisogno di lui", "vieni", "venga", "torna a casa"],
    at_once: &["subito", "adesso", "immediatamente", "urgente"],
    calm: &[
        "non e urgente", "non e un emergenza", "niente di urgente", "non c e fretta", "senza fretta", "nessuna fretta", "quando puoi", "quando hai tempo", "quando puo",
        "notizie", "telegiornale", "esercitazione", "prova antincendio", "falso allarme", "film", "scherzo", "sto scherzando", "muoio di fame",
    ],
    fine: &["niente di grave", "non e niente", "sto bene", "sta bene", "tutto bene", "gia dimesso", "gia dimessa", "stato dimesso", "stata dimessa", "ce la cavo"],
    past: &["ieri", "l altro ieri", "la settimana scorsa", "l anno scorso", "il mese scorso", "anni fa", "un anno fa", "da un anno", "mesi fa", "l altro giorno", "anniversario"],
    now: &["appena", "proprio ora", "poco fa", "adesso", "subito"],
};

const RU: Cues = Cues {
    events: &[
        "потерял сознание", "потеряла сознание", "упал в обморок", "упала в обморок", "без сознания", "скорая помощь", "скорую", "на скорой",
        "в больницу", "в больнице", "в реанимацию", "в реанимации", "инсульт", "инфаркт", "сердечный приступ", "не дышит",
        "авария", "попал в аварию", "попала в аварию", "сбила машина", "дтп", "на операции",
        "пожар", "горит", "пожарные", "утечка газа", "пахнет газом", "запах газа", "затопило", "наводнение", "взрыв",
        "пропал", "пропала", "потерялся", "потерялась", "не можем найти", "не вернулся", "не вернулась",
        "умер", "умерла", "скончался", "скончалась", "при смерти",
        "ограбили", "обокрали", "взломали", "залезли воры", "вызвали полицию", "вызвал полицию", "вызвала полицию",
        "застрял в лифте", "застряла в лифте", "заблокирован*", "заблудились",
        "украли паспорт", "украли кошелек", "потерял паспорт", "потеряла паспорт", "не могу вернуться",
        "случилась беда", "что то страшное", "система упала", "сервер упал", "серверы упали", "все упало", "производство встало", "производство остановлено",
        "несчастный случай на производстве", "травма на работе",
        "звонили из школы", "позвонили из школы", "разбил голову", "сломал руку", "ударился головой",
    ],
    urge: &["срочно", "это срочно", "очень срочно", "чрезвычайная ситуация", "экстренно", "помогите", "на помощь", "нужен прямо сейчас", "приезжай немедленно", "возвращайся немедленно"],
    ask: &["позовите его", "передайте ему", "пусть позвонит", "мне нужно с ним поговорить", "приезжай", "приезжайте", "возвращайся"],
    at_once: &["прямо сейчас", "немедленно", "срочно", "сейчас же"],
    calm: &[
        "не срочно", "ничего срочного", "не торопись", "не спеши", "без спешки", "когда будет время", "когда сможешь",
        "новости", "в новостях", "учения", "учебная тревога", "ложная тревога", "фильм", "шутка", "шучу", "умираю с голоду",
    ],
    fine: &["ничего серьезного", "ничего страшного", "все хорошо", "со мной все в порядке", "уже выписали", "выписался", "выписалась", "справляюсь"],
    past: &["вчера", "позавчера", "на прошлой неделе", "в прошлом году", "в прошлом месяце", "год назад", "лет назад", "уже год", "месяцев назад", "на днях", "годовщина"],
    now: &["только что", "прямо сейчас", "сейчас же"],
};

const ID: Cues = Cues {
    events: &[
        "pingsan", "tidak sadarkan diri", "ambulans", "ambulan", "igd", "ugd", "dibawa ke rumah sakit", "masuk rumah sakit", "masuk icu",
        "stroke", "serangan jantung", "tidak bernapas", "kecelakaan", "tabrakan", "ketabrak", "ditabrak", "sedang dioperasi",
        "kebakaran", "terbakar", "pemadam kebakaran", "bocor gas", "gas bocor", "bau gas", "banjir", "kebanjiran", "ledakan",
        "hilang", "tersesat", "belum pulang", "tidak ketemu",
        "meninggal", "baru saja meninggal", "sekarat", "kritis",
        "dirampok", "kemalingan", "pencurian", "lapor polisi", "panggil polisi",
        "terjebak di lift", "terkurung di lift", "terjebak", "tersesat di gunung",
        "kehilangan paspor", "kehilangan dompet", "paspor hilang", "dompet hilang", "dicuri", "tidak bisa pulang",
        "ada musibah", "sesuatu yang buruk", "sistem down", "server down", "sistem mati", "server mati", "produksi berhenti",
        "kecelakaan kerja", "kecelakaan di pabrik", "terluka di tempat kerja",
        "pihak sekolah menelepon", "sekolah menelepon", "jatuh di sekolah", "patah tangan", "kepalanya terbentur",
    ],
    urge: &["darurat", "ini darurat", "sangat mendesak", "mendesak", "tolong", "butuh kamu sekarang", "segera datang", "pulang sekarang", "cepat ke sini"],
    ask: &["sambungkan ke", "bilang ke dia", "suruh dia", "hubungi saya", "saya perlu bicara", "datang ke", "pulang"],
    at_once: &["sekarang juga", "segera", "langsung", "sekarang", "cepat"],
    calm: &[
        "tidak darurat", "bukan darurat", "tidak mendesak", "tidak usah buru buru", "tidak buru buru", "santai saja", "kapan sempat", "kapan ada waktu",
        "berita", "simulasi", "latihan kebakaran", "alarm palsu", "film", "bercanda", "canda", "kelaparan", "mati kelaparan",
    ],
    fine: &["tidak parah", "tidak apa apa", "bukan apa apa", "baik baik saja", "sudah pulang dari rumah sakit", "sudah keluar dari rumah sakit", "sudah sembuh"],
    past: &["kemarin", "minggu lalu", "tahun lalu", "bulan lalu", "bertahun tahun lalu", "setahun lalu", "sudah setahun", "beberapa bulan lalu", "ulang tahun", "peringatan"],
    now: &["baru saja", "barusan", "sekarang juga", "tadi"],
};

const TH: Cues = Cues {
    events: &[
        "เป็นลม", "หมดสติ", "รถพยาบาล", "ห้องฉุกเฉิน", "ส่งโรงพยาบาล", "เข้าโรงพยาบาล", "เส้นเลือดในสมอง", "หัวใจวาย", "ไม่หายใจ", "ไอซียู",
        "อุบัติเหตุ", "รถชน", "ถูกรถชน", "กำลังผ่าตัด",
        "ไฟไหม้", "ดับเพลิง", "แก๊สรั่ว", "ได้กลิ่นแก๊ส", "น้ำท่วม", "ระเบิด",
        "หายตัว", "หาไม่เจอ", "พลัดหลง", "ยังไม่กลับบ้าน",
        "เสียชีวิต", "เพิ่งเสีย", "กำลังจะตาย", "อาการโคม่า",
        "โดนปล้น", "ถูกปล้น", "ขโมยขึ้นบ้าน", "แจ้งตำรวจ", "เรียกตำรวจ",
        "ติดอยู่ในลิฟต์", "ติดลิฟต์", "ติดอยู่", "หลงป่า", "หลงทาง",
        "กระเป๋าหาย", "พาสปอร์ตหาย", "โดนขโมย", "กลับบ้านไม่ได้",
        "เกิดเรื่องใหญ่", "ระบบล่ม", "เซิร์ฟเวอร์ล่ม", "ระบบพัง", "สายการผลิตหยุด",
        "อุบัติเหตุในโรงงาน", "อุบัติเหตุที่ทำงาน", "โรงเรียนโทรมา", "หกล้มหัวกระแทก", "แขนหัก", "บาดเจ็บ",
    ],
    urge: &["ฉุกเฉิน", "ด่วนมาก", "เรื่องด่วน", "ช่วยด้วย", "รีบมาเดี๋ยวนี้", "ต้องมาตอนนี้", "กลับมาเดี๋ยวนี้"],
    ask: &[],
    at_once: &[],
    calm: &[
        "ไม่ด่วน", "ไม่ได้ด่วน", "ไม่รีบ", "ไม่ต้องรีบ", "ว่างค่อย", "ว่างเมื่อไหร่",
        "ข่าว", "ซ้อม", "ฝึกซ้อม", "สัญญาณเตือนปลอม", "ล้อเล่น", "หนัง", "ละคร", "หิวจะตาย", "หิวตายแล้ว",
    ],
    fine: &["ไม่เป็นไร", "ไม่มีอะไรร้ายแรง", "สบายดี", "ออกจากโรงพยาบาลแล้ว", "หายดีแล้ว"],
    past: &["เมื่อวาน", "วันก่อน", "สัปดาห์ที่แล้ว", "อาทิตย์ที่แล้ว", "ปีที่แล้ว", "เดือนที่แล้ว", "หลายปีก่อน", "หนึ่งปีแล้ว", "ครบรอบ"],
    now: &["เมื่อกี้", "เพิ่ง", "ตอนนี้เลย", "เดี๋ยวนี้"],
};

const VI: Cues = Cues {
    events: &[
        "ngat xiu", "bat tinh", "xe cap cuu", "cap cuu", "phong cap cuu", "dua vao benh vien", "nhap vien", "dot quy", "nhoi mau co tim", "ngung tho", "hoi suc tich cuc",
        "tai nan giao thong", "tai nan xe", "bi tai nan", "bi xe tong", "bi tong xe", "dang phau thuat", "dang mo",
        "chay nha", "boc chay", "hoa hoan", "cuu hoa", "linh cuu hoa", "ro ri ga", "ro ri gas", "mui ga", "ngap lut", "vu no",
        "bi mat tich", "mat tich", "di lac", "khong tim thay", "chua ve nha",
        "qua doi", "moi mat", "vua mat", "hap hoi", "nguy kich",
        "bi cuop", "cuop giat", "trom dot nhap", "bao cong an", "goi cong an", "goi canh sat",
        "ket trong thang may", "mac ket", "lac tren nui",
        "mat ho chieu", "mat vi", "bi mat cap", "khong co cach nao ve",
        "co chuyen lon", "co chuyen chang lanh", "he thong sap", "he thong bi sap", "may chu sap", "server sap", "san xuat dung",
        "tai nan lao dong", "tai nan o nha may", "bi thuong khi lam viec",
        "truong goi", "nha truong goi", "nga dap dau", "gay tay", "bi thuong o truong",
    ],
    urge: &["khan cap", "rat gap", "viec gap", "cuu voi", "can anh ngay", "ve ngay", "den ngay lap tuc"],
    ask: &["noi voi anh ay", "chuyen may cho", "bao anh ay", "can gap", "ve nha", "den day"],
    at_once: &["ngay bay gio", "ngay lap tuc", "lap tuc", "gap lam"],
    calm: &[
        "khong gap", "khong khan cap", "khong co gi gap", "khong voi", "cu tu tu", "luc nao ranh", "khi nao ranh",
        "tin tuc", "dien tap", "bao chay gia", "phim", "dua thoi", "noi dua", "chet doi",
    ],
    fine: &["khong sao", "khong co gi nghiem trong", "on roi", "da xuat vien", "xuat vien roi", "khoe roi"],
    past: &["hom qua", "hom kia", "tuan truoc", "nam ngoai", "thang truoc", "cach day vai nam", "mot nam truoc", "ngay gio", "ky niem"],
    now: &["vua moi", "moi day", "ngay bay gio", "vua xay ra"],
};

const HI: Cues = Cues {
    events: &[
        "बेहोश", "होश नहीं", "एम्बुलेंस", "इमरजेंसी", "आपातकालीन", "अस्पताल ले जा", "अस्पताल में भर्ती", "आईसीयू",
        "दिल का दौरा", "हार्ट अटैक", "स्ट्रोक", "सांस नहीं", "एक्सीडेंट", "दुर्घटना", "कार से टक्कर", "गाड़ी से टक्कर", "ऑपरेशन चल रहा",
        "आग लग गई", "आग लगी", "दमकल", "फायर ब्रिगेड", "गैस लीक", "गैस की बदबू", "बाढ़", "धमाका", "विस्फोट",
        "लापता", "गुम हो", "खो गया", "खो गई", "मिल नहीं रह", "घर नहीं लौटा", "घर नहीं लौटी",
        "निधन हो गया", "गुजर गए", "गुजर गईं", "दम तोड़", "हालत गंभीर",
        "लूटपाट", "चोरी हो गई", "डाका", "पुलिस को बुला", "पुलिस को फोन",
        "लिफ्ट में फंस", "फंस गया", "फंस गई", "पहाड़ पर भटक",
        "पासपोर्ट चोरी", "बटुआ चोरी", "पर्स चोरी", "वापस आने का रास्ता नहीं",
        "बहुत बड़ी मुसीबत", "कुछ बुरा हुआ", "सिस्टम क्रैश", "सर्वर डाउन", "सर्वर बंद", "प्रोडक्शन रुक",
        "काम पर हादसा", "फैक्ट्री में हादसा", "स्कूल से फोन", "सिर में चोट", "हाथ टूट गया", "चोट लग गई",
    ],
    urge: &["इमरजेंसी है", "बहुत जरूरी है", "बहुत ज़रूरी है", "तुरंत आओ", "तुरंत आइए", "अभी आओ", "अभी आइए", "बचाओ", "मदद करो"],
    ask: &["बात करा दीजिए", "उनसे कहिए", "उन्हें बता दीजिए", "अभी आना", "घर आओ", "यहाँ आओ"],
    at_once: &["तुरंत", "अभी", "फौरन", "फ़ौरन"],
    calm: &[
        "जल्दी नहीं", "जरूरी नहीं", "ज़रूरी नहीं", "कोई जल्दी नहीं", "आराम से", "फुर्सत में", "जब समय मिले",
        "समाचार", "खबर", "ख़बर", "मॉक ड्रिल", "अभ्यास", "झूठा अलार्म", "फिल्म", "फ़िल्म", "मजाक", "मज़ाक", "भूख से मर",
    ],
    fine: &["कुछ गंभीर नहीं", "सब ठीक है", "मैं ठीक हूँ", "छुट्टी मिल गई", "अस्पताल से छुट्टी", "ठीक हो गए"],
    past: &["कल", "परसों", "पिछले हफ्ते", "पिछले हफ़्ते", "पिछले साल", "पिछले महीने", "साल पहले", "एक साल हो गया", "बरसी", "सालगिरह"],
    now: &["अभी अभी", "तुरंत", "अभी"],
};

const AR: Cues = Cues {
    events: &[
        "اغمي عليه", "اغمي عليها", "مغمى عليه", "فاقد الوعي", "فاقده الوعي", "سياره اسعاف", "الاسعاف", "الطوارئ", "قسم الطوارئ",
        "نقلوه للمستشفي", "نقلوها للمستشفي", "في المستشفي", "العنايه المركزه", "جلطه", "سكته قلبيه", "ازمه قلبيه", "لا يتنفس",
        "حادث سياره", "تعرض لحادث", "حادث مروري", "صدمته سياره", "في غرفه العمليات",
        "حريق", "تشتعل", "الدفاع المدني", "المطافئ", "تسرب غاز", "رائحه غاز", "غرق", "فيضان", "انفجار",
        "مفقود", "مفقوده", "تاه", "ضاع", "ما لقيناه", "ما لقيناها", "لم يعد للمنزل",
        "توفي", "توفيت", "مات", "يحتضر", "حالته حرجه",
        "سرقه", "سطو", "اقتحام", "اتصلنا بالشرطه", "بلغنا الشرطه",
        "عالق في المصعد", "عالقه في المصعد", "محبوس", "تائهين",
        "انسرق جواز سفري", "انسرقت محفظتي", "ضاع جوازي", "ما عندي طريقه ارجع",
        "مصيبه في البيت", "شيء خطير", "النظام تعطل", "السيرفر طاح", "السيرفر متوقف", "توقف الانتاج",
        "اصابه عمل", "حادث في المصنع", "المدرسه اتصلت", "اتصلوا من المدرسه", "انكسرت يده", "انكسرت يدها", "انضرب في راسه",
    ],
    urge: &["طوارئ", "حاله طارئه", "امر عاجل", "عاجل جدا", "النجده", "ساعدوني", "نحتاجك فورا", "تعال فورا", "ارجع فورا"],
    ask: &["اعطني اياه", "قل له", "اطلب منه", "احتاج اكلمه", "تعال", "ارجع للبيت"],
    at_once: &["فورا", "الان", "حالا"],
    calm: &[
        "ليس عاجل", "مش مستعجل", "مو مستعجل", "لا تستعجل", "على مهلك", "متي ما فضيت",
        "الاخبار", "تدريب", "بروفه", "انذار كاذب", "فيلم", "امزح", "نكته", "اموت من الجوع", "ميت من الجوع",
    ],
    fine: &["لا شيء خطير", "ما في شيء خطير", "انا بخير", "بخير الان", "خرج من المستشفي", "خرجت من المستشفي", "تعافي"],
    past: &["امس", "اول امس", "الاسبوع الماضي", "السنه الماضيه", "العام الماضي", "الشهر الماضي", "منذ سنوات", "قبل سنه", "الذكرى السنويه"],
    now: &["للتو", "قبل قليل", "الان", "فورا"],
};

const ALL_CUES: [&Cues; 15] = [
    &ZH, &JA, &KO, &EN, &ES, &FR, &DE, &PT, &IT, &RU, &ID, &TH, &VI, &HI, &AR,
];

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

/// CJK ideographs, kana, hangul and spaceless scripts (Thai, Lao, Myanmar, Khmer):
/// phrases without spaces between words.
fn is_wide(c: char) -> bool {
    (c as u32) >= 0x2E80 || matches!(c as u32, 0x0E00..=0x0EFF | 0x1000..=0x109F | 0x1780..=0x17FF)
}

fn has_cjk_or_corner(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c as u32, 0x3400..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2EBEF)
            || matches!(c, '「' | '」' | '『' | '』')
    })
}

fn is_word_script_char(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(c as u32, 0x0900..=0x0963 | 0x0966..=0x097F | 0x0E01..=0x0E5B)
}

/// Latin letters without accents, full-width ASCII as ASCII, simplified Chinese
/// (already folded via OpenCC `to_cn`), Arabic/Russian folds; anything else not
/// belonging to a word becomes a space.
fn fold_char(c: char, out: &mut String) {
    let c = match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        '\u{3000}' => ' ',
        _ => c,
    };
    for c in c.to_lowercase() {
        let base = match c {
            // A combining accent (e + U+0301) is no letter of its own, nor Arabic harakat.
            '\u{0300}'..='\u{036F}' | '\u{064B}'..='\u{065F}' | '\u{0670}' => continue,
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => "a",
            'ă' | 'ạ' | 'ả' | 'ấ' | 'ầ' | 'ẩ' | 'ẫ' | 'ậ' | 'ắ' | 'ằ' | 'ẳ' | 'ẵ' | 'ặ' => "a",
            'ç' => "c",
            'đ' => "d",
            'è' | 'é' | 'ê' | 'ë' => "e",
            'ẹ' | 'ẻ' | 'ẽ' | 'ế' | 'ề' | 'ể' | 'ễ' | 'ệ' => "e",
            'ì' | 'í' | 'î' | 'ï' => "i",
            'ĩ' | 'ỉ' | 'ị' => "i",
            'ñ' => "n",
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => "o",
            'ơ' | 'ọ' | 'ỏ' | 'ố' | 'ồ' | 'ổ' | 'ỗ' | 'ộ' | 'ớ' | 'ờ' | 'ở' | 'ỡ' | 'ợ' => "o",
            'ù' | 'ú' | 'û' | 'ü' => "u",
            'ũ' | 'ư' | 'ụ' | 'ủ' | 'ứ' | 'ừ' | 'ử' | 'ữ' | 'ự' => "u",
            'ý' | 'ÿ' => "y",
            'ỳ' | 'ỵ' | 'ỷ' | 'ỹ' => "y",
            'ß' => "ss",
            'œ' => "oe",
            'ё' => "е",
            'أ' | 'إ' | 'آ' | 'ٱ' => "ا",
            'ة' => "ه",
            'ى' => "ي",
            _ => {
                // Live speech-to-text homophones for 掛 (OpenCC preserves valid 卦/褂).
                let c = match c {
                    '卦' | '褂' => '挂',
                    _ => c,
                };
                out.push(if is_word_script_char(c) { c } else { ' ' });
                continue;
            }
        };
        out.push_str(base);
    }
}

/// The folded text with single spaces between words and one at both ends (so
/// " word " matches whole words).
pub(crate) fn fold_spaced(s: &str) -> String {
    let owned;
    let text = if has_cjk_or_corner(s) {
        owned = crate::zh_convert::to_cn(s);
        owned.as_str()
    } else {
        s
    };
    let mut raw = String::with_capacity(text.len() + 2);
    for c in text.chars() {
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
    text.split(['.', '!', '?', '。', '！', '？', '\n', '…', ';', '；', '।', '؟'])
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
/// others ([read_language]).
pub fn caller_language(said: &str) -> Option<Lang> {
    let tail: String = {
        let chars: Vec<char> = said.chars().collect();
        chars[chars.len().saturating_sub(300)..].iter().collect()
    };
    read_language(&tail).map(|r| r.lang)
}

/// What a stretch of speech is in ([read_language]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub lang: Lang,
    /// Said as a whole in it: enough of it, and it makes up most of what was
    /// said (a stray "OK", a name or a few kana in Chinese leave it unclear).
    pub clear: bool,
}

/// Latin letters (accented ones too).
fn is_latin(c: char) -> bool {
    c.is_ascii_alphabetic()
        || (matches!(c as u32, 0xC0..=0x24F | 0x1E00..=0x1EFF) && c != '\u{D7}' && c != '\u{F7}')
}

fn is_han_bmp(c: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&c)
}

fn zh_variant(said: &str) -> Lang {
    let cn = crate::zh_convert::to_cn(said);
    let tw = crate::zh_convert::to_tw(said);
    let trad = if said.chars().count() == cn.chars().count() {
        said.chars().zip(cn.chars()).filter(|(a, b)| a != b && is_han_bmp(*a)).count()
    } else {
        usize::from(said != cn)
    };
    let simp = if said.chars().count() == tw.chars().count() {
        said.chars()
            .zip(tw.chars())
            .filter(|(a, b)| {
                a != b
                    && is_han_bmp(*a)
                    && !matches!(*a, '里' | '周' | '后' | '台' | '才' | '只' | '面' | '系' | '干')
            })
            .count()
    } else {
        usize::from(said != tw)
    };
    if simp > trad { Lang::ZhCn } else { Lang::ZhTw }
}

/// The language of what was said, by its share of each script. Kana or Hangul
/// count only when they make up a fair share of the Chinese characters around
/// them (speech-to-text writes a stray kana or two into Chinese, Taiwanese or a
/// noise); Chinese, Japanese or Korean only when its characters outweigh the
/// Latin words (an English sentence with a Chinese name stays English, a
/// Chinese one with "OK" or "LINE" stays Chinese); a Latin language only with
/// two of its common words, or one in two words or more, and not when stronger
/// markers of an unlisted Latin language (Vietnamese, Italian, Indonesian,
/// Turkish, Polish, Dutch) outweigh it.
pub fn read_language(said: &str) -> Option<Reading> {
    let (mut hangul, mut kana, mut han) = (0usize, 0usize, 0usize);
    for c in said.chars() {
        match c as u32 {
            0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => hangul += 1,
            // Not 「・」, which Chinese writes between the parts of a foreign name.
            0x3040..=0x309F | 0x30A1..=0x30FA | 0x30FC => kana += 1,
            0x4E00..=0x9FFF => han += 1,
            _ => {}
        }
    }
    let words = said
        .split(|c: char| !(is_latin(c) || c == '\''))
        .filter(|t| t.chars().any(is_latin))
        .count();
    let cjk = hangul + kana + han;
    // A Latin word against about two Chinese characters.
    let total = cjk + 2 * words;
    let clear_share = |n: usize| n >= 4 && n * 3 >= total * 2;
    let cjk_reading = || -> Option<Reading> {
        if hangul >= 2 && hangul * 2 >= cjk {
            return Some(Reading { lang: Lang::Ko, clear: clear_share(hangul + han) });
        }
        if kana >= 2 && kana * 4 >= kana + han {
            return Some(Reading { lang: Lang::Ja, clear: clear_share(kana + han) });
        }
        if han >= 2 {
            return Some(Reading { lang: zh_variant(said), clear: clear_share(cjk) });
        }
        None
    };
    if cjk >= 2 && cjk >= 2 * words {
        if let Some(r) = cjk_reading() {
            return Some(r);
        }
    }
    if !is_strong_other_latin(said) {
        if let Some((lang, top)) = latin_scores(said) {
            if top >= 2 || (top == 1 && words >= 2) {
                return Some(Reading { lang, clear: top >= 2 && words >= 3 && clear_share(2 * words) });
            }
        }
    }
    cjk_reading().map(|r| Reading { clear: false, ..r })
}

/// The caller's words are in a language without a setting of its own (Thai,
/// Russian, Italian, Vietnamese, Indonesian…): no Chinese, Japanese or Korean
/// letters, none of the setting languages [caller_language] tells, and more
/// than an "OK" — four letters of another script, or three words of Latin letters.
pub fn unlisted_language(said: &str) -> bool {
    let said = said.trim();
    if said.is_empty() || caller_language(said).is_some() {
        return false;
    }
    let other_latin = is_strong_other_latin(said);
    if !other_latin && latin_scores(said).is_some() {
        return false;
    }
    let (mut cjk, mut latin, mut other) = (0, 0, 0);
    for c in said.chars() {
        match c as u32 {
            0x3040..=0x30FF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => cjk += 1,
            0x41..=0x5A | 0x61..=0x7A | 0xC0..=0x24F | 0x1E00..=0x1EFF => latin += 1,
            _ if c.is_alphabetic() => other += 1,
            _ => {}
        }
    }
    let words = said.split_whitespace().filter(|w| w.chars().any(char::is_alphabetic)).count();
    let unique_script = other_latin && other_latin_stats(said).unique_script;
    // Another script: most of the letters (a Thai word in an English sentence is not enough).
    cjk == 0 && ((other >= 4 && other * 2 > latin) || (latin >= 12 && words >= 3) || (unique_script && words >= 3))
}

#[derive(Default)]
struct OtherLatin {
    best: i32,
    best_words: i32,
    unique_script: bool,
    unique_tokens: usize,
}

fn is_vi_unique_char(c: char) -> bool {
    matches!(
        c,
        'đ' | 'Đ'
            | 'ơ'
            | 'Ơ'
            | 'ư'
            | 'Ư'
            | 'ă'
            | 'Ă'
            | '\u{1EA0}'..='\u{1EF9}'
            | '\u{0306}'
            | '\u{031B}'
            | '\u{0323}'
            | '\u{0309}'
    )
}

fn is_tr_unique_char(c: char) -> bool {
    matches!(c, 'ğ' | 'Ğ' | 'ı' | 'İ' | 'ş' | 'Ş')
}

fn is_pl_unique_char(c: char) -> bool {
    matches!(
        c,
        'ł' | 'Ł' | 'ą' | 'Ą' | 'ę' | 'Ę' | 'ż' | 'Ż' | 'ź' | 'Ź' | 'ć' | 'Ć' | 'ń' | 'Ń' | 'ś' | 'Ś'
    )
}

fn other_latin_stats(text: &str) -> OtherLatin {
    const VI_WORDS: &[&str] = &[
        "xin", "chào", "tôi", "không", "được", "vâng", "người", "nhé", "rồi", "giúp", "mình", "bạn", "anh", "chị",
        "cảm", "ơn", "bưu", "kiện", "giao", "hàng", "nhà", "tên", "của", "với", "này", "gọi", "khẩn", "cấp", "ngay",
        "bệnh", "viện", "cứu", "tai", "nạn", "cháy", "công", "an", "cảnh", "sát",
    ];
    const IT_WORDS: &[&str] = &[
        "sono", "ciao", "pronto", "questo", "questa", "questi", "queste", "però", "perché", "perche", "grazie",
        "dalla", "della", "delle", "degli", "dello", "nella", "nelle", "negli", "nello", "sulla", "vorrei", "chiamo",
        "chiami", "signor", "signora", "signore", "pacco", "pacchi", "corriere", "consegna", "portineria", "posso",
        "può", "abbiamo", "siamo", "tutto", "tutti", "molto", "anche", "sempre", "subito", "adesso", "domani", "oggi",
        "allora", "quindi", "nessuno", "qualcuno", "qualcosa", "bisogno", "scusi", "scusa", "prego", "arrivederci",
        "arrivederla", "buongiorno", "buonasera", "giornata", "serata", "richiamo", "richiamare", "lasciare", "lascio",
        "lascerò", "parlo", "parlare", "per", "più", "già", "così", "cioè", "ospedale", "ambulanza", "incidente",
        "polizia", "emergenza", "fuoco", "successo", "svenuto", "svenuta", "mamma", "papà", "fratello", "sorella",
        "bambino", "bambina",
    ];
    const ID_WORDS: &[&str] = &[
        "saya", "anda", "bapak", "ibu", "tolong", "terima", "kasih", "makasih", "paket", "paketnya", "sudah", "udah",
        "tidak", "nggak", "bisa", "ada", "ini", "itu", "dari", "untuk", "yang", "dengan", "mau", "akan", "rumah",
        "kurir", "telepon", "telpon", "maaf", "permisi", "selamat", "pagi", "siang", "sore", "malam", "besok",
        "sekarang", "nanti", "belum", "jangan", "kirim", "antar", "titip", "ambil", "orang", "kami", "kita", "mereka",
        "beliau", "mbak", "halo", "segera", "darurat", "kecelakaan", "kebakaran", "pingsan", "ambulans", "sakit",
        "polisi",
    ];
    const TR_WORDS: &[&str] = &[
        "merhaba", "evet", "hayır", "hayir", "teşekkür", "tesekkur", "teşekkürler", "lütfen", "lutfen", "kargo",
        "kargonuz", "evde", "misiniz", "arıyorum", "ariyorum", "benim", "sizin", "acil", "hastane", "ambulans",
        "kaza", "yangın", "yangin", "polis", "şimdi", "simdi", "yok", "değil", "degil", "günler", "gunler", "iyi",
        "tamam", "efendim", "kapıda", "kapida", "bayıldı", "bayildi",
    ];
    const PL_WORDS: &[&str] = &[
        "dzień", "dzien", "dobry", "cześć", "czesc", "dziękuję", "dziekuje", "proszę", "prosze", "paczka", "paczkę",
        "paczke", "kurier", "jestem", "jest", "czy", "pani", "domu", "można", "mozna", "zostawić", "zostawic",
        "pilne", "wypadek", "karetka", "szpital", "policja", "pożar", "pozar", "natychmiast",
    ];
    const NL_WORDS: &[&str] = &[
        "ik", "het", "een", "van", "voor", "niet", "zijn", "heb", "heeft", "bent", "thuis", "wij", "mijn", "jij",
        "goedemorgen", "goedemiddag", "goedenavond", "dank", "bedankt", "alstublieft", "alsjeblieft", "pakket",
        "pakketje", "bezorger", "bezorging", "bellen", "bel", "maar", "naar", "ook", "nog", "geen", "ziekenhuis",
        "ongeluk", "spoed", "dringend", "meteen", "politie", "brandweer", "flauwgevallen",
    ];
    // [Vi, It, Id, Tr, Pl, Nl]
    let mut scores = [0i32; 6];
    let mut word_hits = [0i32; 6];
    let mut unique_tokens = [0usize; 6];
    for raw_tok in text
        .split(|c: char| !(c.is_alphabetic() || matches!(c, '\'' | '\u{2019}' | '\u{2018}' | '\u{60}') || ('\u{0300}'..='\u{036F}').contains(&c)))
        .filter(|t| !t.is_empty())
    {
        if raw_tok.chars().any(is_vi_unique_char) {
            unique_tokens[0] += 1;
        }
        if raw_tok.chars().any(is_tr_unique_char) {
            unique_tokens[3] += 1;
        }
        if raw_tok.chars().any(is_pl_unique_char) {
            unique_tokens[4] += 1;
        }
        let tok: String = raw_tok
            .to_lowercase()
            .chars()
            .filter(|c| *c != '\u{0307}')
            .map(|c| if matches!(c, '\u{2019}' | '\u{2018}' | '\u{60}') { '\'' } else { c })
            .collect();
        let word = tok.as_str();
        for (idx, list) in [VI_WORDS, IT_WORDS, ID_WORDS, TR_WORDS, PL_WORDS, NL_WORDS].iter().enumerate() {
            if list.contains(&word) {
                scores[idx] += 1;
                word_hits[idx] += 1;
            }
        }
        if matches!(word, "è" | "c'è") {
            scores[1] += 2;
        }
    }
    for c in text.chars() {
        if is_vi_unique_char(c) {
            scores[0] += 2;
        } else if matches!(c, 'ò' | 'Ò') {
            scores[1] += 2;
        } else if is_tr_unique_char(c) {
            scores[3] += 2;
        } else if is_pl_unique_char(c) {
            scores[4] += 2;
        }
    }
    let Some((best_idx, &best)) = scores.iter().enumerate().max_by_key(|(_, s)| **s) else {
        return OtherLatin::default();
    };
    OtherLatin {
        best,
        best_words: word_hits[best_idx],
        unique_script: unique_tokens[best_idx] > 0,
        unique_tokens: unique_tokens[best_idx],
    }
}

fn is_strong_other_latin(text: &str) -> bool {
    let (_, total_scores, word_scores) = latin_word_and_total_scores(text);
    let setting_total = total_scores.into_iter().max().unwrap_or(0);
    let setting_words = word_scores.into_iter().max().unwrap_or(0);
    let o = other_latin_stats(text);
    if o.best < 2 {
        return false;
    }
    if o.unique_script
        && (o.unique_tokens >= 2 || o.best_words >= 1 || setting_words == 0)
        && o.best > setting_words
    {
        return true;
    }
    o.best > setting_total
}

/// Score of the strongest unlisted Latin language (`it, vi, id, tr, pl, nl`) when
/// it clearly outweighs the five setting Latin languages; `0` otherwise.
pub(crate) fn unlisted_latin_best_score(text: &str) -> i32 {
    if is_strong_other_latin(text) {
        other_latin_stats(text).best
    } else {
        0
    }
}

/// The Latin language with the most common words and letters in `text`, and
/// its score; None when no language scores.
fn latin_scores(text: &str) -> Option<(Lang, i32)> {
    let (langs, score) = latin_score_table(text);
    let (best, top) = score.iter().enumerate().max_by_key(|(_, s)| **s)?;
    (*top > 0).then_some((langs[best], *top))
}

/// How much more a text reads as `lang` than as `other` (their common words
/// and letters, [latin_score_table]); 0 for languages not in the table. Close
/// languages share words (`en`, `la`, `no`): a short Spanish line can score a
/// point of Portuguese, so a switch between them needs a clear margin.
pub fn latin_margin(text: &str, lang: Lang, other: Lang) -> i32 {
    let (langs, score) = latin_score_table(text);
    let of = |l: Lang| langs.iter().position(|x| *x == l).map_or(0, |i| score[i]);
    of(lang) - of(other)
}

/// The Latin-script languages [latin_scores] tells apart.
pub fn is_latin_lang(lang: Lang) -> bool {
    matches!(lang, Lang::En | Lang::Es | Lang::Fr | Lang::De | Lang::Pt)
}

fn latin_score_table(text: &str) -> ([Lang; 5], [i32; 5]) {
    let (langs, total, _) = latin_word_and_total_scores(text);
    (langs, total)
}

fn latin_word_and_total_scores(text: &str) -> ([Lang; 5], [i32; 5], [i32; 5]) {
    const WORDS: [(Lang, &[&str]); 5] = [
        (Lang::En, &["the", "and", "is", "are", "was", "i", "i'm", "you", "he", "she", "it", "to", "my", "his", "her", "in", "on", "at", "of", "just", "there", "has", "been", "have", "need", "we", "our", "someone", "with", "for", "they're", "it's", "get", "back", "call", "whenever", "so", "what", "this", "that", "your", "can", "will"]),
        (Lang::Es, &["el", "la", "los", "las", "y", "en", "se", "mi", "me", "es", "está", "por", "una", "un", "lo", "le", "ya", "hay", "del", "al", "muy", "con", "no", "estoy", "nos"]),
        (Lang::Fr, &["le", "la", "les", "est", "et", "je", "il", "elle", "on", "du", "des", "une", "un", "pas", "dans", "à", "au", "aux", "s'est", "c'est", "j'ai", "mon", "ma", "ne", "qu'il", "depuis", "suis"]),
        (Lang::De, &["der", "die", "das", "ist", "ich", "und", "nicht", "er", "sie", "es", "ein", "eine", "einen", "im", "zu", "mit", "hat", "wir", "mein", "meine", "bei", "uns", "sich", "sofort", "schon", "vom", "seit", "wurde", "wurden", "hab"]),
        (Lang::Pt, &["o", "os", "as", "e", "em", "do", "da", "no", "na", "meu", "minha", "um", "uma", "não", "tá", "tô", "pra", "pro", "com", "ele", "ela", "já", "foi", "é", "eu", "tão", "ao", "hoje", "teve", "você", "gente", "isso", "bora", "ontem", "dela", "dele"]),
    ];
    let lower = text.to_lowercase().replace(['\u{2019}', '\u{2018}', '\u{60}'], "'");
    let mut word_score = [0i32; 5];
    let mut total_score = [0i32; 5];
    for token in lower.split(|c: char| !(c.is_alphabetic() || c == '\'')).filter(|t| !t.is_empty()) {
        for (i, (_, words)) in WORDS.iter().enumerate() {
            if words.contains(&token) {
                word_score[i] += 1;
                total_score[i] += 1;
            }
        }
        if ["l'", "d'", "qu'", "j'", "s'", "c'", "m'", "n'"].iter().any(|p| token.starts_with(p) && token.len() > p.len()) {
            total_score[2] += 1;
        }
    }
    for c in lower.chars() {
        match c {
            'ñ' | '¿' | '¡' => total_score[1] += 2,
            'ã' | 'õ' => total_score[4] += 2,
            'ç' => {
                total_score[2] += 1;
                total_score[4] += 1;
            }
            'ß' | 'ä' | 'ö' | 'ü' => total_score[3] += 2,
            'è' | 'ê' | 'à' | 'ù' | 'î' | 'ô' | 'û' | 'œ' | 'ë' => total_score[2] += 1,
            'á' | 'í' | 'ó' | 'ú' => {
                total_score[1] += 1;
                total_score[4] += 1;
            }
            _ => {}
        }
    }
    (WORDS.map(|(l, _)| l), total_score, word_score)
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

    #[test]
    fn a_language_without_a_setting_is_told_apart() {
        for t in [
            "สวัสดีครับ ผมเป็นคนส่งของ",
            "Алло, это курьер, вы дома?",
            "Halo, saya kurir paket, bapak ada di rumah?",
            "مرحبا، أنا من شركة التوصيل",
            "Ciao, sono Marco, ho un pacco per lei",
            "Xin chào, tôi là nhân viên giao hàng ạ",
            "Merhaba efendim, kargonuz kapıda",
            "Dzień dobry, mam paczkę dla pani, czy jest ktoś w domu?",
            "Goedemorgen, ik heb een pakketje voor u",
            "नमस्ते, मैं डिलीवरी वाला हूँ, क्या कोई घर पर है?",
        ] {
            assert!(unlisted_language(t), "{t}");
        }
        for t in [
            "",
            "OK OK",
            "喂，你好",
            "Hello, I'm the courier, where should I leave it?",
            "もしもし",
            "여보세요 택배입니다",
            "OK，那我放管理室",
            "Hola, tengo un paquete para usted, ¿lo dejo en portería?",
            "Bonjour, je suis le livreur, vous êtes chez vous ?",
            "Hallo, ich bin der Paketbote, sind Sie zu Hause?",
            "Olá, sou o entregador, posso deixar com o porteiro?",
        ] {
            assert!(!unlisted_language(t), "{t}");
        }
    }

    const LANGS: [&str; 16] = [
        "zh-TW", "zh-CN", "en", "ja", "ko", "es", "fr", "de", "pt",
        "it", "ru", "id", "th", "vi", "hi", "ar",
    ];

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
        ("it", 0, "Mia mamma è svenuta in casa e l'ambulanza la sta portando al pronto soccorso adesso!"),
        ("it", 1, "Mio fratello ha avuto un incidente stradale molto grave, i pompieri lo stanno tirando fuori, vieni subito!"),
        ("it", 2, "La cucina sta andando a fuoco, abbiamo già chiamato i vigili del fuoco, vieni subito!"),
        ("it", 3, "Non troviamo più il bambino al centro commerciale, è scomparso e abbiamo bisogno di aiuto subito!"),
        ("it", 4, "Purtroppo mia nonna è morta poco fa in ospedale, però hanno detto di venire subito adesso."),
        ("it", 5, "Sono entrati i ladri in casa e ci hanno derubato, abbiamo già chiamato la polizia!"),
        ("it", 6, "Sono rimasto bloccato in ascensore da quasi un'ora e non risponde nessuno, ho bisogno di aiuto!"),
        ("it", 7, "Sono ancora all'estero, però mi hanno rubato il passaporto e il portafoglio, proprio non ho come tornare oggi!"),
        ("it", 8, "È successo qualcosa di grave a casa, digli di tornare subito adesso!"),
        ("it", 9, "Il sistema è bloccato però tutta la produzione è ferma adesso, gli dica subito che abbiamo bisogno di lui!"),
        ("it", 10, "Hanno chiamato da scuola, il bambino ha battuto la testa e adesso lo portano in ospedale!"),
        ("ru", 0, "Мама только что упала в обморок дома, скорая помощь уже везет ее в больницу, приезжай немедленно!"),
        ("ru", 1, "Брат попал в аварию, машина разбита, врачи уже на месте, приезжай прямо сейчас!"),
        ("ru", 2, "На кухне пожар, все горит, пожарные уже едут!"),
        ("ru", 3, "Ребенок пропал в торговом центре, мы никак не можем найти его!"),
        ("ru", 4, "Бабушка только что скончалась в больнице, врачи просят приехать прямо сейчас."),
        ("ru", 5, "В квартиру залезли воры и все ограбили, я уже вызвал полицию!"),
        ("ru", 6, "Я застрял в лифте уже почти час, никак не могу выбраться, помогите!"),
        ("ru", 7, "У меня в поездке украли паспорт и украли кошелек, не могу вернуться домой!"),
        ("ru", 8, "Дома случилась беда, передайте ему, пусть возвращается прямо сейчас!"),
        ("ru", 9, "Сервер упал и производство встало, ему нужно все срочно проверить прямо сейчас!"),
        ("ru", 10, "Позвонили из школы, сын упал на физкультуре и разбил голову, везут в больницу!"),
        ("id", 0, "Ibu saya baru saja pingsan di rumah dan ambulans sedang membawanya ke rumah sakit sekarang!"),
        ("id", 1, "Adik saya mengalami kecelakaan lalu lintas, sekarang masih di UGD, tolong segera datang!"),
        ("id", 2, "Dapur kami mengalami kebakaran besar, pemadam kebakaran sudah menuju ke sini!"),
        ("id", 3, "Anak kami hilang di mal dan sampai sekarang belum ketemu juga, tolong bantu kami!"),
        ("id", 4, "Nenek baru saja meninggal di rumah sakit, keluarga diminta segera datang sekarang!"),
        ("id", 5, "Rumah kami baru saja dirampok dan saya sudah lapor polisi, tolong pulang sekarang!"),
        ("id", 6, "Saya terjebak di lift sudah hampir satu jam dan tidak bisa keluar, tolong bantu saya!"),
        ("id", 7, "Saya di luar negeri, saya kehilangan paspor dan dompet, saya tidak bisa pulang sekarang!"),
        ("id", 8, "Di rumah ada musibah besar, bilang ke dia untuk segera datang sekarang juga!"),
        ("id", 9, "Server down dan sistem kami mati sehingga produksi berhenti sekarang juga, ini bilang ke dia saya butuh bantuan!"),
        ("id", 10, "Sekolah menelepon memberi tahu anak kami jatuh di sekolah dan patah tangan, sekarang saya sudah panik di sini!"),
        ("th", 0, "แม่เป็นลมอยู่ที่บ้านตอนนี้ รถพยาบาลกำลังพาไปส่งโรงพยาบาล รีบมาเดี๋ยวนี้เลย!"),
        ("th", 1, "น้องเกิดอุบัติเหตุรถชน ตอนนี้กำลังผ่าตัดอยู่ รีบมาเดี๋ยวนี้!"),
        ("th", 2, "ในครัวเกิดไฟไหม้ รถดับเพลิงกำลังมาแล้ว รีบกลับมาเดี๋ยวนี้!"),
        ("th", 3, "ลูกหายตัวไปที่ห้าง ยังหาไม่เจอเลย ช่วยด้วย!"),
        ("th", 4, "คุณยายเพิ่งเสียชีวิตที่โรงพยาบาล หมอบอกต้องมาตอนนี้เลย!"),
        ("th", 5, "มีขโมยขึ้นบ้านเมื่อกี้ ฉันแจ้งตำรวจแล้ว รีบกลับมาเดี๋ยวนี้!"),
        ("th", 6, "ฉันติดอยู่ในลิฟต์มาเกือบชั่วโมงแล้ว ออกไม่ได้ ช่วยด้วย!"),
        ("th", 7, "ฉันอยู่ที่สนามบิน พาสปอร์ตหายกับกระเป๋าหาย กลับบ้านไม่ได้เลย!"),
        ("th", 8, "ที่บ้านเกิดเรื่องใหญ่แล้ว บอกให้เขากลับมาเดี๋ยวนี้เลย!"),
        ("th", 9, "ระบบล่มหมดแล้ว สายการผลิตหยุดทำงาน เรื่องด่วนมาก!"),
        ("th", 10, "โรงเรียนโทรมาบอกว่าน้องหกล้มหัวกระแทก ตอนนี้กำลังส่งโรงพยาบาล!"),
        ("vi", 0, "Mẹ tôi vừa mới bị ngất xỉu ở nhà, xe cấp cứu đang đưa vào bệnh viện rồi, anh đến ngay lập tức!"),
        ("vi", 1, "Em trai bị tai nạn giao thông, hiện đang phẫu thuật trong bệnh viện, anh đến ngay lập tức!"),
        ("vi", 2, "Trong bếp đang bốc cháy rồi, lính cứu hỏa đã tới nơi, anh về ngay!"),
        ("vi", 3, "Con bé đi lạc ở siêu thị, mãi vẫn không tìm thấy, anh ơi giúp với!"),
        ("vi", 4, "Bà ngoại vừa mất ở bệnh viện rồi, bác sĩ bảo gia đình đến ngay lập tức!"),
        ("vi", 5, "Nhà mình vừa bị cướp đột nhập, tôi đã gọi công an rồi, anh về ngay!"),
        ("vi", 6, "Tôi đang bị kẹt trong thang máy gần một tiếng rồi không ra được, cứu với!"),
        ("vi", 7, "Tôi đang ở sân bay, bị mất hộ chiếu và mất ví rồi, không có cách nào về nước cả!"),
        ("vi", 8, "Ở nhà có chuyện lớn rồi, bảo anh ấy về ngay lập tức!"),
        ("vi", 9, "Hệ thống bị sập rồi, sản xuất dừng hết cả, bảo anh ấy kiểm tra ngay lập tức!"),
        ("vi", 10, "Nhà trường gọi báo cháu bị ngã đập đầu ở trường, đang đưa vào bệnh viện cấp cứu!"),
        ("hi", 0, "माँ घर पर बेहोश हो गई हैं, एम्बुलेंस अस्पताल ले जा रही है, आप तुरंत आइए!"),
        ("hi", 1, "भाई का बड़ा एक्सीडेंट हो गया है, उसका ऑपरेशन चल रहा है, आप अभी आइए!"),
        ("hi", 2, "रसोई में आग लग गई है, दमकल की गाड़ी आ रही है, तुरंत घर आओ!"),
        ("hi", 3, "बच्चा बाजार में गुम हो गया है, हमें कहीं मिल नहीं रहा है, मदद करो!"),
        ("hi", 4, "दादी का अभी अभी निधन हो गया है, डॉक्टर ने परिवार को तुरंत बुलाया है!"),
        ("hi", 5, "हमारे घर में चोरी हो गई है, मैंने पुलिस को फोन कर दिया है, आप तुरंत आइए!"),
        ("hi", 6, "मैं एक घंटे से लिफ्ट में फंस गया हूँ, बाहर नहीं निकल पा रहा हूँ, बचाओ!"),
        ("hi", 7, "मेरा पासपोर्ट चोरी हो गया और बटुआ चोरी हो गया, मेरे पास वापस आने का रास्ता नहीं है!"),
        ("hi", 8, "घर पर बहुत बड़ी मुसीबत आ गई है, उन्हें बता दीजिए कि तुरंत अभी आना है!"),
        ("hi", 9, "पूरा सिस्टम क्रैश हो गया है और प्रोडक्शन रुक गया है, उनसे कहिए तुरंत देखें!"),
        ("hi", 10, "स्कूल से फोन आया है, बच्चे को सिर में चोट लग गई है और अस्पताल ले जा रहे हैं!"),
        ("ar", 0, "أمي أغمي عليها في البيت الآن وسيارة الإسعاف تنقلها إلى المستشفى، تعال فوراً!"),
        ("ar", 1, "أخي تعرض لحادث سيارة خطير وهو الآن في غرفة العمليات، تعال فوراً!"),
        ("ar", 2, "المطبخ يشتعل وفيه حريق كبير، الدفاع المدني في الطريق الآن!"),
        ("ar", 3, "الطفل مفقود في المركز التجاري وما لقيناه في أي مكان!"),
        ("ar", 4, "جدتي توفيت للتو في المستشفى، والأطباء يطلبون من العائلة الحضور فوراً."),
        ("ar", 5, "اقتحم اللصوص بيتنا ووقعت سرقة، اتصلنا بالشرطة الآن، ارجع فوراً!"),
        ("ar", 6, "أنا عالق في المصعد منذ ساعة تقريباً ولا أستطيع الخروج، النجدة!"),
        ("ar", 7, "أنا في المطار، انسرق جواز سفري وانسرقت محفظتي، ما عندي طريقة أرجع!"),
        ("ar", 8, "حدثت مصيبة في البيت الآن، قل له أن يرجع للبيت فوراً!"),
        ("ar", 9, "النظام تعطل وتوقف الإنتاج بالكامل، اطلب منه أن يتدخل فوراً!"),
        ("ar", 10, "المدرسة اتصلت الآن، الولد سقط في الملعب وانكسرت يده وفي طريقه إلى المستشفى!"),
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
        ("it", 0, "La settimana scorsa sono andato all'ospedale per prendere le medicine, però niente di grave."),
        ("it", 1, "Stamattina abbiamo fatto una prova antincendio in ufficio, però era tutto tranquillo oggi."),
        ("it", 2, "Al telegiornale hanno detto che ieri c'è stato un brutto incidente, però per fortuna sto sempre bene."),
        ("it", 3, "Mia nonna è deceduta l'anno scorso, però oggi parliamo sempre di lei."),
        ("it", 4, "Questo periodo al lavoro è sempre tutto urgente, però ce la cavo bene oggi."),
        ("it", 5, "Ragazzi, muoio di fame adesso, però ceniamo tutti insieme stasera?"),
        ("it", 6, "Non è urgente per niente, però puoi richiamare domani quando hai tempo."),
        ("it", 7, "Proprio adesso sono al supermercato, però ti richiamo subito dopo pranzo."),
        ("ru", 0, "На прошлой неделе я ходил в больницу за лекарствами, ничего серьезного."),
        ("ru", 1, "Утром на работе была учебная тревога, все вышли на улицу."),
        ("ru", 2, "В новостях сказали, что вчера на трассе была авария, будь осторожен."),
        ("ru", 3, "Моя бабушка умерла в прошлом году, сегодня ровно год."),
        ("ru", 4, "На работе сейчас все в спешке, вообще ничего не успеваю."),
        ("ru", 5, "Ой, умираю с голоду, пойдем сегодня ужинать вместе?"),
        ("ru", 6, "Это не срочно, перезвони, когда будет время."),
        ("ru", 7, "Я прямо сейчас в магазине, перезвоню тебе попозже."),
        ("id", 0, "Minggu lalu saya pergi ke rumah sakit untuk ambil obat, tidak parah kok."),
        ("id", 1, "Tadi pagi saya dan bapak ibu di kantor ikut simulasi latihan kebakaran, sudah selesai."),
        ("id", 2, "Menurut berita kemarin ada kecelakaan di jalan tol, namun ibu bapak semua sudah baik baik saja."),
        ("id", 3, "Nenek saya meninggal tahun lalu, hari ini adalah peringatan satu tahun beliau."),
        ("id", 4, "Sekarang pekerjaan di kantor padat sekali dan semua harus cepat, saya sampai bingung."),
        ("id", 5, "Saya sudah mati kelaparan nih, nanti malam kita makan bersama yuk!"),
        ("id", 6, "Ini tidak mendesak kok, bapak telepon saya lagi nanti kapan sempat saja."),
        ("id", 7, "Saya sekarang juga masih di pasar, nanti saya telepon balik ya."),
        ("th", 0, "สัปดาห์ที่แล้วฉันไปรับยาที่โรงพยาบาล ไม่มีอะไรร้ายแรงหรอก"),
        ("th", 1, "เมื่อเช้าที่ทำงานมีการฝึกซ้อมหนีไฟ ทุกคนลงมารวมกันด้านล่าง"),
        ("th", 2, "ในข่าวบอกว่าเมื่อวานมีอุบัติเหตุบนทางด่วน ขับรถระวังนะ"),
        ("th", 3, "คุณย่าเสียชีวิตปีที่แล้ว วันนี้ครบรอบหนึ่งปีพอดี"),
        ("th", 4, "ช่วงนี้งานเร่งทุกอย่างเลย ยุ่งมากจริง ๆ"),
        ("th", 5, "หิวจะตายอยู่แล้ว เย็นนี้ไปกินข้าวด้วยกันไหม"),
        ("th", 6, "ไม่ด่วนหรอก ว่างเมื่อไหร่ค่อยโทรกลับมาก็ได้"),
        ("th", 7, "ตอนนี้เลยฉันยังซื้อของอยู่ เดี๋ยวโทรกลับนะ"),
        ("vi", 0, "Tuần trước tôi có vào bệnh viện lấy thuốc thôi, không có gì nghiêm trọng đâu."),
        ("vi", 1, "Sáng nay công ty có buổi diễn tập phòng cháy chữa cháy, mọi người đều ổn rồi."),
        ("vi", 2, "Tin tức hôm qua đưa có tai nạn trên cao tốc đấy, anh đi đường cẩn thận nhé."),
        ("vi", 3, "Bà nội tôi qua đời năm ngoái rồi, hôm nay là ngày giỗ của bà."),
        ("vi", 4, "Dạo này công việc ở văn phòng bận rộn quá, việc gì cũng dồn dập cả."),
        ("vi", 5, "Trời ơi chết đói mất rồi, tối nay đi ăn cơm cùng nhau nhé?"),
        ("vi", 6, "Chuyện này không gấp đâu, khi nào rảnh thì anh gọi lại cho tôi nhé."),
        ("vi", 7, "Ngay bây giờ tôi đang ở siêu thị, chút nữa tôi gọi lại cho bạn nhé."),
        ("hi", 0, "पिछले हफ्ते मैं अस्पताल से दवा लेने गया था, कुछ गंभीर नहीं है।"),
        ("hi", 1, "आज सुबह ऑफिस में आग बुझाने का मॉक ड्रिल अभ्यास था, सब ठीक है।"),
        ("hi", 2, "समाचार में बताया कि कल हाईवे पर दुर्घटना हुई थी, संभल कर चलना।"),
        ("hi", 3, "पिछले साल दादाजी गुजर गए थे, आज उनकी बरसी है।"),
        ("hi", 4, "आजकल काम बहुत ज्यादा है, हर काम जल्दी निपटाना पड़ रहा है।"),
        ("hi", 5, "अरे मैं तो भूख से मर रहा हूँ, आज शाम को साथ खाना खाएं क्या?"),
        ("hi", 6, "कोई जल्दी नहीं है, जब समय मिले तब वापस फोन कर लेना।"),
        ("hi", 7, "मैं अभी बाज़ार में हूँ, थोड़ी देर में फोन करता हूँ।"),
        ("ar", 0, "الأسبوع الماضي ذهبت إلى المستشفى لأخذ الدواء، لا شيء خطير."),
        ("ar", 1, "هذا الصباح كان عندنا تدريب بروفة إخلاء حريق في العمل، الكل بخير."),
        ("ar", 2, "ذكروا في الأخبار أن هناك حادث سيارة أمس على الطريق السريع، انتبه."),
        ("ar", 3, "جدتي توفيت السنة الماضية، واليوم الذكرى السنوية لوفاتها."),
        ("ar", 4, "هذه الأيام كل شيء في العمل مضغوط ومستعجل."),
        ("ar", 5, "والله أموت من الجوع الآن، هل نتعشى معاً الليلة؟"),
        ("ar", 6, "الأمر ليس عاجل أبداً، اتصل بي متى ما فضيت."),
        ("ar", 7, "أنا الآن في السوق، سأتصل بك لاحقاً."),
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
    fn gas_smell_work_accidents_system_crashes_and_past_deaths_are_told_apart() {
        for s in [
            "你家門口瓦斯味超重，消防隊在路上了",
            "grave accident à l'usine… il faut que tu viennes tout de suite",
            "the whole system just crashed… I need you right now",
            "Minha avó acabou de falecer… Preciso muito de você aqui agora",
            "実は海外でパスポートと財布を盗られちゃって。今どうしようもなくて困ってるんだよ。",
        ] {
            assert!(is_urgent(s), "should be urgent: {s}");
        }
        for s in [
            "it's been a year since my grandfather passed away, but I'm managing okay",
            "It's been a year since my grandfather passed away.",
            "我爺爺去年過世了，現在想起來還是有點難過。",
            "其实今天是外奶奶去世一周年。的日子,我心里有点感触。",
            "他上禮拜已經出院了，現在在家休息。",
            "Écoute, c'est pas une urgence, mais c'est juste que je pensais à ma grand-mère. C'est son anniversaire aujourd'hui. Un an après son décès.",
            "なんか急ぎの仕事ばっかりって感じで慌ただしいけど。どれも緊急ってわけじゃないんだよね。",
            "Cara, ontem teve um alarme de incêndio lá no seu prédio, né?",
        ] {
            assert!(!is_urgent(s), "should not be urgent: {s}");
        }
    }

    #[test]
    fn the_callers_language_is_told_from_what_they_said() {
        let setting_langs = ["zh-TW", "zh-CN", "en", "ja", "ko", "es", "fr", "de", "pt"];
        let wrong: Vec<String> = URGENT
            .iter()
            .chain(NOT_URGENT.iter().filter(|(_, k, _)| *k != 7))
            .filter_map(|(lang, _, s)| {
                if setting_langs.contains(lang) {
                    let got = caller_language(s).map(|l| if l.is_zh() { "zh" } else { l.code() });
                    let want = if lang.starts_with("zh") { "zh" } else { lang };
                    (got != Some(want)).then(|| format!("{lang} got {got:?}: {s}"))
                } else {
                    let got = caller_language(s);
                    let unlisted = unlisted_language(s);
                    (got.is_some() || !unlisted).then(|| format!("unlisted {lang} got {got:?} unlisted={unlisted}: {s}"))
                }
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        assert_eq!(caller_language(""), None);
        assert_eq!(caller_language("123 !!"), None);
    }

    #[test]
    fn traditional_chinese_urgent_words_are_normalized_via_opencc() {
        for s in [
            "我媽媽剛剛在家裡暈倒了，救護車剛到！",
            "爸爸突然心臟病發送急診，你快來醫院！",
            "隔壁房子著火了，消防車已經趕過來！",
            "廚房突然瓦斯外洩，大家都在撤離，快回來！",
            "弟弟打翻熱湯嚴重燙傷，現在在醫院急診！",
            "爺爺從早上失蹤到現在還沒找到，已經報警了！",
            "這件事十萬火急，請務必馬上回電給我！",
            "我在機場護照被偷，沒辦法搭機，回不了國了！",
            "公司整個系統的網站掛了，全部客戶都停擺！",
            "線上大故障，客戶群那邊都炸鍋了，快回來處理！",
        ] {
            assert!(is_urgent(s), "zh-TW should be urgent via to_cn: {s}");
        }
        assert_eq!(caller_language("我媽媽暈倒了，已經送醫院急診！"), Some(Lang::ZhTw));
        assert_eq!(caller_language("台灣台北的快遞到了請問有人在家嗎？"), Some(Lang::ZhTw));
        assert_eq!(caller_language("我爸在家晕倒了，已经叫了救护车送急诊！"), Some(Lang::ZhCn));
    }

    #[test]
    fn unlisted_latin_languages_never_mislock_as_setting_latin_languages() {
        for sample in [
            "Ciao, sono Marco, ho un pacco per lei, posso lasciarlo in portineria?",
            "Pronto, vorrei parlare con il signor Rossi per una consegna urgente.",
            "Xin chào, tôi là nhân viên giao hàng, anh có ở nhà không ạ?",
            "Tôi muốn gửi bưu kiện cho anh, cảm ơn nhiều nhé.",
            "Halo, saya kurir paket, bapak atau ibu ada di rumah sekarang?",
            "Terima kasih, paketnya sudah saya titipkan ke satpam ya.",
            "Merhaba efendim, kargonuz kapıda bekliyor, evde misiniz?",
            "Dzień dobry, dzwonię z firmy kurierskiej, mam paczkę dla pani.",
            "Goedemorgen, ik bel voor een bezorging en heb een pakketje bij me.",
        ] {
            assert!(unlisted_language(sample), "expected unlisted=true: {sample}");
            assert_eq!(caller_language(sample), None, "expected caller_language=None: {sample}");
            assert_eq!(read_language(sample), None, "expected read_language=None: {sample}");
        }
        for (lang, sample) in [
            (Lang::En, "Hello, I have a package for you, can I leave it at the front desk?"),
            (Lang::Es, "Hola, soy el repartidor, tengo un paquete para usted hoy."),
            (Lang::Fr, "Bonjour, je suis le livreur, vous êtes chez vous pour le colis ?"),
            (Lang::De, "Guten Tag, ich habe ein Paket für Sie, sind Sie zu Hause?"),
            (Lang::Pt, "Olá, sou o entregador, posso deixar o pacote com o porteiro?"),
        ] {
            assert!(!unlisted_language(sample), "expected unlisted=false for {lang:?}: {sample}");
            assert_eq!(caller_language(sample), Some(lang), "expected {lang:?}: {sample}");
        }
    }

    #[test]
    fn folding_reads_spaces_accents_and_widths_alike() {
        assert_eq!(fold_compact("Ne raccroche pas"), fold_compact("neraccroche pas"));
        assert_eq!(fold_compact("Não desliga"), "naodesliga");
        assert_eq!(fold_compact("請你先別走"), "请你先别走");
        assert_eq!(fold_compact("ＤＯＮ\u{2019}Ｔ HANG UP"), "donthangup");
        assert_eq!(fold_spaced("Don't hang-up!"), " don t hang up ");
    }

    #[test]
    fn folding_drops_combining_accents_and_vietnamese_tone_marks() {
        assert_eq!(fold_compact("Ne quitte\u{301} pas"), fold_compact("ne quitté pas"));
        assert_eq!(fold_compact("Đừng cúp máy"), "dungcupmay");
        assert_eq!(fold_compact("D\u{323}u\u{303}ng cu\u{301}p ma\u{301}y"), fold_compact("dung cup may"));
    }

    #[test]
    fn everyday_words_alone_are_not_an_emergency() {
        for said in [
            "Le Wi-Fi est en panne chez moi, rappelle-moi plus tard.",
            "Mon stylo est tombé derrière le canapé.",
            "Kannst du mir kurz Feuer geben?",
            "我的耳机不见了，你有看到吗？",
            "老師打來說下週要交報名表。",
            "老师打来问你下周有没有空。",
        ] {
            assert!(!is_urgent(said), "{said}");
        }
        // The emergencies those words belong to still are.
        for said in [
            "Mamie est tombée dans l'escalier, elle ne bouge plus !",
            "Le serveur est en panne, toute la production est à l'arrêt.",
            "In der Küche ist Feuer ausgebrochen, die Feuerwehr ist da!",
            "小孩不见了，到处都找不到，你快回来！",
            "我爸爸不見了，已經報警了。",
        ] {
            assert!(is_urgent(said), "{said}");
        }
    }
}
