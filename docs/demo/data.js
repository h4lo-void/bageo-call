// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

// Illustrative, prewritten conversations. No live calls or AI requests.
window.BAGEO_DEMO = {
  "en": {
    "name": "English",
    "greeting": "Hello?",
    "tagline": "Your number. A little less ringing.",
    "hero": [
      "Your phone rings.",
      "Bageo takes it."
    ],
    "intro": "An AI that answers and makes calls on your own number. In nine languages.",
    "choose": "Pick a call",
    "language": "Choose a language",
    "simulation": "Scripted interactive demo",
    "start": "Let Bageo answer",
    "next": "Next line",
    "replay": "Play again",
    "reset": "Reset",
    "incoming": "Incoming call",
    "outgoing": "Ready to call",
    "active": "Bageo is talking",
    "done": "Your call, summed up",
    "caller": "Caller",
    "assistant": "Bageo",
    "summary": "What matters",
    "progress": "Conversation progress",
    "setup": "Get started",
    "source": "View on GitHub",
    "requirements": "Rooted Android + SIM + computer + Gemini key",
    "notice": "Scripted examples. No phone calls, microphone access or AI requests. Actual responses vary.",
    "auto": "Auto-play",
    "pause": "Pause",
    "call": "Let Bageo call",
    "scenes": [
      {
        "id": "delivery",
        "title": "Package delivery",
        "teaser": "A package arrives while you’re busy.",
        "description": "Bageo gets the details. You decide what happens next.",
        "outcome": "Signature needed. Another delivery is possible before 5 pm; confirm a time yourself.",
        "task": "",
        "lines": [
          [
            "caller",
            "Your package is here."
          ],
          [
            "assistant",
            "Hi, I’m an AI assistant answering this phone. Does it need a signature in person?"
          ],
          [
            "caller",
            "Yes. We can deliver again before five this afternoon."
          ],
          [
            "assistant",
            "I’ll pass that on and ask the recipient to confirm a time."
          ]
        ]
      },
      {
        "id": "sales",
        "title": "Sales pitch",
        "teaser": "A “special offer” is calling.",
        "description": "Bageo asks the questions. Your private details stay out of this demo.",
        "outcome": "Kept asking for details. No personal information was shared in this example.",
        "task": "",
        "lines": [
          [
            "caller",
            "You’re approved for a low-interest loan today!"
          ],
          [
            "assistant",
            "Is that a monthly interest rate or an annual one?"
          ],
          [
            "caller",
            "First, give me your personal details."
          ],
          [
            "assistant",
            "How do you calculate the fees? How much is the monthly payment?"
          ]
        ]
      },
      {
        "id": "booking",
        "title": "Book a table",
        "teaser": "One instruction. One less call.",
        "description": "Give Bageo a task and your limits. It calls on your behalf.",
        "outcome": "Example result: tomorrow, 7:30 pm, four people. Within your instructions.",
        "task": "Book four seats tomorrow at 7 pm. 7:30 is fine too.",
        "lines": [
          [
            "assistant",
            "Hello, I’m an AI assistant calling to book a table for four tomorrow at seven."
          ],
          [
            "caller",
            "Seven is full. We have a table at seven thirty."
          ],
          [
            "assistant",
            "Seven thirty works. Please book it for four, thank you."
          ],
          [
            "caller",
            "Confirmed: tomorrow at seven thirty, four people."
          ]
        ]
      }
    ]
  },
  "zh-TW": {
    "name": "繁體中文",
    "greeting": "喂？",
    "tagline": "你的門號，八哥接手。",
    "hero": [
      "電話響了？",
      "交給八哥。"
    ],
    "intro": "接電話、替你打電話，還會講九種語言。用的就是你原本的門號。",
    "choose": "選一通電話",
    "language": "換個語言試試",
    "simulation": "互動情境示範 · 非真實通話",
    "start": "交給八哥接",
    "next": "下一句",
    "replay": "再玩一次",
    "reset": "重新開始",
    "incoming": "來電中",
    "outgoing": "準備代打",
    "active": "八哥通話中",
    "done": "重點幫你記好了",
    "caller": "對方",
    "assistant": "八哥",
    "summary": "這通電話的重點",
    "progress": "對話進度",
    "setup": "開始使用",
    "source": "到 GitHub 看看",
    "requirements": "備用 Android（需 root）＋ SIM ＋電腦＋ Gemini 金鑰",
    "notice": "預寫對話示範，不會撥電話、使用麥克風或呼叫 AI。實際回應依情境而異。",
    "auto": "自動播放",
    "pause": "暫停播放",
    "call": "讓八哥代打",
    "scenes": [
      {
        "id": "delivery",
        "title": "包裹來電",
        "teaser": "包裹到了，但你正忙。",
        "description": "八哥問清楚、記重點，要怎麼處理由你決定。",
        "outcome": "包裹需本人簽收，下午五點前可再配送；等你確認時間。",
        "task": "",
        "lines": [
          [
            "caller",
            "你的包裹到了。"
          ],
          [
            "assistant",
            "你好，我是 AI 助理，幫忙接電話。包裹需要本人簽收嗎？"
          ],
          [
            "caller",
            "需要，今天下午五點前都能再配送。"
          ],
          [
            "assistant",
            "我會轉告，請本人再跟你確認時間。"
          ]
        ]
      },
      {
        "id": "sales",
        "title": "推銷陪聊",
        "teaser": "「有個超優惠方案」打來了。",
        "description": "讓八哥一題接一題問，這段示範裡不提供任何個資。",
        "outcome": "繼續問細節；示範中沒有提供個資。",
        "task": "",
        "lines": [
          [
            "caller",
            "有一筆低利貸款，今天就能辦！"
          ],
          [
            "assistant",
            "利息是月息還是年息？"
          ],
          [
            "caller",
            "先提供你的身分資料。"
          ],
          [
            "assistant",
            "先說說手續費怎麼算？每個月要付多少？"
          ]
        ]
      },
      {
        "id": "booking",
        "title": "代打訂位",
        "teaser": "交代一句，少打一通。",
        "description": "說清楚任務和可接受的範圍，八哥替你打。",
        "outcome": "示範結果：明晚 19:30，四位；時間符合你的交代。",
        "task": "幫我訂明晚四位，七點或七點半都可以。",
        "lines": [
          [
            "assistant",
            "你好，我是 AI 助理，想訂明晚七點四位。"
          ],
          [
            "caller",
            "七點滿了，七點半可以。"
          ],
          [
            "assistant",
            "七點半也可以，麻煩訂四位，謝謝。"
          ],
          [
            "caller",
            "好的，明晚七點半四位。"
          ]
        ]
      }
    ]
  },
  "zh-CN": {
    "name": "简体中文",
    "greeting": "喂？",
    "tagline": "你的号码，八哥接手。",
    "hero": [
      "电话响了？",
      "交给八哥。"
    ],
    "intro": "接电话、替你打电话，还会讲九种语言。用的就是你原本的号码。",
    "choose": "选一通电话",
    "language": "换个语言试试",
    "simulation": "互动情境演示 · 非真实通话",
    "start": "交给八哥接",
    "next": "下一句",
    "replay": "再玩一次",
    "reset": "重新开始",
    "incoming": "来电中",
    "outgoing": "准备代打",
    "active": "八哥通话中",
    "done": "重点帮你记好了",
    "caller": "对方",
    "assistant": "八哥",
    "summary": "这通电话的重点",
    "progress": "对话进度",
    "setup": "开始使用",
    "source": "到 GitHub 看看",
    "requirements": "备用 Android（需 root）＋ SIM ＋电脑＋ Gemini 密钥",
    "notice": "预写对话演示，不会拨电话、使用麦克风或调用 AI。实际回应因情境而异。",
    "auto": "自动播放",
    "pause": "暂停播放",
    "call": "让八哥代打",
    "scenes": [
      {
        "id": "delivery",
        "title": "包裹来电",
        "teaser": "包裹到了，但你正忙。",
        "description": "八哥问清楚、记重点，要怎么处理由你决定。",
        "outcome": "包裹需本人签收，下午五点前可再配送；等你确认时间。",
        "task": "",
        "lines": [
          [
            "caller",
            "你的包裹到了。"
          ],
          [
            "assistant",
            "你好，我是 AI 助理，帮忙接电话。包裹需要本人签收吗？"
          ],
          [
            "caller",
            "需要，今天下午五点前都能再配送。"
          ],
          [
            "assistant",
            "我会转告，请本人再跟你确认时间。"
          ]
        ]
      },
      {
        "id": "sales",
        "title": "推销陪聊",
        "teaser": "“有个超优惠方案”打来了。",
        "description": "让八哥一题接一题问，这段演示里不提供任何个人信息。",
        "outcome": "继续问细节；演示中没有提供个人信息。",
        "task": "",
        "lines": [
          [
            "caller",
            "有一笔低息贷款，今天就能办！"
          ],
          [
            "assistant",
            "利息是月息还是年息？"
          ],
          [
            "caller",
            "先提供你的身份信息。"
          ],
          [
            "assistant",
            "先说说手续费怎么算？每个月要付多少？"
          ]
        ]
      },
      {
        "id": "booking",
        "title": "代打订位",
        "teaser": "交代一句，少打一通。",
        "description": "说清楚任务和可接受的范围，八哥替你打。",
        "outcome": "演示结果：明晚 19:30，四位；时间符合你的交代。",
        "task": "帮我订明晚四位，七点或七点半都可以。",
        "lines": [
          [
            "assistant",
            "你好，我是 AI 助理，想订明晚七点四位。"
          ],
          [
            "caller",
            "七点满了，七点半可以。"
          ],
          [
            "assistant",
            "七点半也可以，麻烦订四位，谢谢。"
          ],
          [
            "caller",
            "好的，明晚七点半四位。"
          ]
        ]
      }
    ]
  },
  "ja": {
    "name": "日本語",
    "greeting": "もしもし？",
    "tagline": "いつもの番号に、頼れる八哥。",
    "hero": [
      "電話が来たら、",
      "八哥におまかせ。"
    ],
    "intro": "いつもの電話番号で、AIが電話を受けたり、かけたり。9言語に対応。",
    "choose": "電話を選ぶ",
    "language": "言語を変えてみる",
    "simulation": "台本によるデモ · 実際の通話ではありません",
    "start": "八哥に出てもらう",
    "next": "次のひと言",
    "replay": "もう一度",
    "reset": "最初から",
    "incoming": "着信中",
    "outgoing": "発信の準備",
    "active": "八哥が通話中",
    "done": "要点をまとめました",
    "caller": "相手",
    "assistant": "八哥",
    "summary": "通話の要点",
    "progress": "会話の進行",
    "setup": "使ってみる",
    "source": "GitHubで見る",
    "requirements": "root化済みAndroid ＋ SIM ＋ パソコン ＋ Geminiキー",
    "notice": "用意された会話のデモです。発信、マイク使用、AIへの接続は行いません。実際の応答は状況により異なります。",
    "auto": "自動再生",
    "pause": "一時停止",
    "call": "八哥にかけてもらう",
    "scenes": [
      {
        "id": "delivery",
        "title": "荷物の配達",
        "teaser": "荷物が到着。でも今は手が離せない。",
        "description": "八哥が用件を確認。どうするかはあなたが決めます。",
        "outcome": "本人のサインが必要。本日17時まで再配達可能。時間の確認が必要です。",
        "task": "",
        "lines": [
          [
            "caller",
            "お荷物をお届けに来ました。"
          ],
          [
            "assistant",
            "こんにちは。AIアシスタントが代わりに電話に出ています。本人のサインが必要ですか？"
          ],
          [
            "caller",
            "はい。本日午後5時までなら再配達できます。"
          ],
          [
            "assistant",
            "本人に伝えて、時間を確認してもらいます。"
          ]
        ]
      },
      {
        "id": "sales",
        "title": "営業電話",
        "teaser": "「特別なご案内」がかかってきた。",
        "description": "八哥が次々と質問。このデモでは個人情報を渡しません。",
        "outcome": "詳しい説明を求め続けました。この例では個人情報を渡していません。",
        "task": "",
        "lines": [
          [
            "caller",
            "低金利のローンを本日ご利用いただけます！"
          ],
          [
            "assistant",
            "その金利は月利ですか、年利ですか？"
          ],
          [
            "caller",
            "まず個人情報を教えてください。"
          ],
          [
            "assistant",
            "手数料はどう計算しますか？毎月いくら払うんですか？"
          ]
        ]
      },
      {
        "id": "booking",
        "title": "お店の予約",
        "teaser": "ひと言頼んで、電話をおまかせ。",
        "description": "用件と希望の範囲を伝えると、八哥が代わりに電話します。",
        "outcome": "デモ結果：明日19時30分、4名。指定の時間帯で予約。",
        "task": "明日の夜、4人で予約して。7時か7時半なら大丈夫。",
        "lines": [
          [
            "assistant",
            "こんにちは。AIアシスタントです。明日の夜7時に4名で予約したいのですが。"
          ],
          [
            "caller",
            "7時は満席ですが、7時半なら空いています。"
          ],
          [
            "assistant",
            "7時半で大丈夫です。4名でお願いします。"
          ],
          [
            "caller",
            "承りました。明日の夜7時半、4名様ですね。"
          ]
        ]
      }
    ]
  },
  "ko": {
    "name": "한국어",
    "greeting": "여보세요?",
    "tagline": "내 번호 그대로, 바거가 대신.",
    "hero": [
      "전화가 오면,",
      "바거에게 맡겨요."
    ],
    "intro": "내 전화번호로 AI가 전화를 받고 걸어요. 9개 언어로 대화할 수 있어요.",
    "choose": "전화 고르기",
    "language": "다른 언어로 보기",
    "simulation": "대본 기반 데모 · 실제 통화 아님",
    "start": "바거가 받기",
    "next": "다음 대화",
    "replay": "다시 보기",
    "reset": "처음부터",
    "incoming": "전화가 왔어요",
    "outgoing": "전화 걸 준비",
    "active": "바거가 통화 중",
    "done": "핵심만 정리했어요",
    "caller": "상대방",
    "assistant": "바거",
    "summary": "통화 요약",
    "progress": "대화 진행",
    "setup": "시작하기",
    "source": "GitHub 보기",
    "requirements": "루팅된 Android ＋ SIM ＋ 컴퓨터 ＋ Gemini 키",
    "notice": "미리 작성한 대화입니다. 전화를 걸거나 마이크를 쓰거나 AI에 요청하지 않습니다. 실제 응답은 상황에 따라 달라집니다.",
    "auto": "자동 재생",
    "pause": "일시 정지",
    "call": "바거가 전화 걸기",
    "scenes": [
      {
        "id": "delivery",
        "title": "택배 전화",
        "teaser": "택배가 왔는데 손을 뗄 수 없어요.",
        "description": "바거가 내용을 확인해요. 다음 행동은 직접 결정하세요.",
        "outcome": "본인 서명이 필요해요. 오늘 오후 5시 전 재배송 가능하며 시간 확인이 필요해요.",
        "task": "",
        "lines": [
          [
            "caller",
            "택배 도착했습니다."
          ],
          [
            "assistant",
            "안녕하세요. 대신 전화를 받는 AI 비서입니다. 본인 서명이 필요한가요?"
          ],
          [
            "caller",
            "네. 오늘 오후 다섯 시 전에는 다시 배송할 수 있어요."
          ],
          [
            "assistant",
            "전달하고 본인이 시간을 확인하도록 하겠습니다."
          ]
        ]
      },
      {
        "id": "sales",
        "title": "영업 전화",
        "teaser": "“특별한 혜택”이라는 전화가 왔어요.",
        "description": "바거가 계속 질문해요. 이 데모에서는 개인정보를 주지 않아요.",
        "outcome": "자세한 내용을 계속 물었어요. 이 예시에서는 개인정보를 제공하지 않았어요.",
        "task": "",
        "lines": [
          [
            "caller",
            "오늘 바로 저금리 대출이 가능합니다!"
          ],
          [
            "assistant",
            "월 이자인가요, 연 이자인가요?"
          ],
          [
            "caller",
            "먼저 개인정보를 알려주세요."
          ],
          [
            "assistant",
            "수수료는 어떻게 계산하나요? 매달 얼마를 내나요?"
          ]
        ]
      },
      {
        "id": "booking",
        "title": "예약 전화",
        "teaser": "한마디로 부탁하고, 전화는 맡기세요.",
        "description": "할 일과 허용 범위를 알려주면 바거가 대신 전화해요.",
        "outcome": "데모 결과: 내일 저녁 7시 30분, 4명. 요청한 시간 범위예요.",
        "task": "내일 저녁 네 명 예약해 줘. 일곱 시나 일곱 시 반이면 돼.",
        "lines": [
          [
            "assistant",
            "안녕하세요. AI 비서입니다. 내일 저녁 일곱 시에 네 명 예약하고 싶어요."
          ],
          [
            "caller",
            "일곱 시는 만석이고 일곱 시 반은 가능해요."
          ],
          [
            "assistant",
            "일곱 시 반도 괜찮습니다. 네 명으로 부탁드려요."
          ],
          [
            "caller",
            "네, 내일 저녁 일곱 시 반 네 명 예약했습니다."
          ]
        ]
      }
    ]
  },
  "es": {
    "name": "Español",
    "greeting": "¿Diga?",
    "tagline": "Tu número. Menos interrupciones.",
    "hero": [
      "¿Suena el teléfono?",
      "Que conteste Bageo."
    ],
    "intro": "Una IA que recibe y hace llamadas con tu número de siempre. En nueve idiomas.",
    "choose": "Elige una llamada",
    "language": "Prueba otro idioma",
    "simulation": "Demo con guion · No es una llamada real",
    "start": "Que conteste Bageo",
    "next": "Siguiente frase",
    "replay": "Repetir",
    "reset": "Reiniciar",
    "incoming": "Llamada entrante",
    "outgoing": "Listo para llamar",
    "active": "Bageo está hablando",
    "done": "Lo importante, resumido",
    "caller": "Interlocutor",
    "assistant": "Bageo",
    "summary": "Lo que importa",
    "progress": "Progreso de la conversación",
    "setup": "Empezar",
    "source": "Ver en GitHub",
    "requirements": "Android con root + SIM + ordenador + clave de Gemini",
    "notice": "Ejemplos escritos. Sin llamadas, micrófono ni solicitudes a la IA. Las respuestas reales pueden variar.",
    "auto": "Reproducción automática",
    "pause": "Pausar",
    "call": "Que llame Bageo",
    "scenes": [
      {
        "id": "delivery",
        "title": "Entrega",
        "teaser": "Llega un paquete y estás ocupado.",
        "description": "Bageo pregunta los detalles. Tú decides qué hacer.",
        "outcome": "Hace falta una firma en persona. Otra entrega es posible antes de las 17:00; falta confirmar la hora.",
        "task": "",
        "lines": [
          [
            "caller",
            "Ha llegado su paquete."
          ],
          [
            "assistant",
            "Hola, soy un asistente de IA que atiende este teléfono. ¿Hace falta que el destinatario firme en persona?"
          ],
          [
            "caller",
            "Sí. Podemos volver hoy antes de las cinco de la tarde."
          ],
          [
            "assistant",
            "Se lo comunicaré para que confirme la hora con usted."
          ]
        ]
      },
      {
        "id": "sales",
        "title": "Venta telefónica",
        "teaser": "Llama una “oferta especial”.",
        "description": "Bageo sigue preguntando. Este ejemplo no comparte datos personales.",
        "outcome": "Siguió pidiendo detalles. En este ejemplo no se compartieron datos personales.",
        "task": "",
        "lines": [
          [
            "caller",
            "¡Tiene un préstamo de bajo interés aprobado para hoy!"
          ],
          [
            "assistant",
            "¿Ese interés es mensual o anual?"
          ],
          [
            "caller",
            "Primero, facilíteme sus datos personales."
          ],
          [
            "assistant",
            "¿Cómo se calculan las comisiones? ¿Cuánto hay que pagar al mes?"
          ]
        ]
      },
      {
        "id": "booking",
        "title": "Reservar mesa",
        "teaser": "Una indicación. Una llamada menos.",
        "description": "Dale una tarea y tus límites. Bageo llama por ti.",
        "outcome": "Resultado de ejemplo: mañana a las 19:30, cuatro personas. Dentro de lo autorizado.",
        "task": "Reserva para cuatro mañana a las siete. Las siete y media también me van bien.",
        "lines": [
          [
            "assistant",
            "Hola, soy un asistente de IA. Quisiera reservar para cuatro mañana a las siete."
          ],
          [
            "caller",
            "A las siete está lleno. Hay sitio a las siete y media."
          ],
          [
            "assistant",
            "Las siete y media está bien. Para cuatro, por favor."
          ],
          [
            "caller",
            "Confirmado: mañana a las siete y media, cuatro personas."
          ]
        ]
      }
    ]
  },
  "fr": {
    "name": "Français",
    "greeting": "Allô ?",
    "tagline": "Votre numéro. Moins d’interruptions.",
    "hero": [
      "Le téléphone sonne ?",
      "Bageo répond."
    ],
    "intro": "Une IA qui répond et passe des appels avec votre numéro habituel. En neuf langues.",
    "choose": "Choisir un appel",
    "language": "Essayer une autre langue",
    "simulation": "Démo scénarisée · Pas un appel réel",
    "start": "Laisser Bageo répondre",
    "next": "Phrase suivante",
    "replay": "Rejouer",
    "reset": "Recommencer",
    "incoming": "Appel entrant",
    "outgoing": "Prêt à appeler",
    "active": "Bageo est en ligne",
    "done": "L’essentiel est noté",
    "caller": "Interlocuteur",
    "assistant": "Bageo",
    "summary": "L’essentiel de l’appel",
    "progress": "Progression de la conversation",
    "setup": "Commencer",
    "source": "Voir sur GitHub",
    "requirements": "Android rooté + SIM + ordinateur + clé Gemini",
    "notice": "Exemples écrits. Aucun appel, accès au micro ou requête à l’IA. Les réponses réelles peuvent varier.",
    "auto": "Lecture automatique",
    "pause": "Pause",
    "call": "Laisser Bageo appeler",
    "scenes": [
      {
        "id": "delivery",
        "title": "Livraison",
        "teaser": "Un colis arrive, mais vous êtes occupé.",
        "description": "Bageo prend les détails. Vous décidez de la suite.",
        "outcome": "Signature en personne nécessaire. Nouvelle livraison possible avant 17 h ; horaire à confirmer.",
        "task": "",
        "lines": [
          [
            "caller",
            "Votre colis est arrivé."
          ],
          [
            "assistant",
            "Bonjour, je suis un assistant IA qui répond à ce téléphone. Le destinataire doit-il signer en personne ?"
          ],
          [
            "caller",
            "Oui. Nous pouvons repasser avant dix-sept heures aujourd’hui."
          ],
          [
            "assistant",
            "Je lui transmettrai le message pour qu’il confirme l’heure avec vous."
          ]
        ]
      },
      {
        "id": "sales",
        "title": "Démarchage",
        "teaser": "Une « offre spéciale » vous appelle.",
        "description": "Bageo enchaîne les questions. Aucun renseignement personnel dans cet exemple.",
        "outcome": "A continué à demander des détails. Aucune donnée personnelle partagée dans cet exemple.",
        "task": "",
        "lines": [
          [
            "caller",
            "Un prêt à taux réduit est disponible pour vous aujourd’hui !"
          ],
          [
            "assistant",
            "C’est un taux mensuel ou annuel ?"
          ],
          [
            "caller",
            "Donnez-moi d’abord vos informations personnelles."
          ],
          [
            "assistant",
            "Comment calculez-vous les frais ? Quel est le montant des mensualités ?"
          ]
        ]
      },
      {
        "id": "booking",
        "title": "Réservation",
        "teaser": "Une consigne. Un appel en moins.",
        "description": "Donnez une tâche et vos limites. Bageo appelle pour vous.",
        "outcome": "Résultat de l’exemple : demain à 19 h 30, quatre personnes. Conforme à vos consignes.",
        "task": "Réserve pour quatre demain à 19 h. 19 h 30 me convient aussi.",
        "lines": [
          [
            "assistant",
            "Bonjour, je suis un assistant IA. Je voudrais réserver pour quatre demain à dix-neuf heures."
          ],
          [
            "caller",
            "C’est complet à dix-neuf heures. Il reste une table à dix-neuf heures trente."
          ],
          [
            "assistant",
            "Dix-neuf heures trente convient. Pour quatre, merci."
          ],
          [
            "caller",
            "C’est confirmé : demain à dix-neuf heures trente, quatre personnes."
          ]
        ]
      }
    ]
  },
  "de": {
    "name": "Deutsch",
    "greeting": "Hallo?",
    "tagline": "Deine Nummer. Weniger Unterbrechungen.",
    "hero": [
      "Das Telefon klingelt?",
      "Bageo geht ran."
    ],
    "intro": "Eine KI nimmt Anrufe an und ruft für dich an. Mit deiner Nummer, in neun Sprachen.",
    "choose": "Anruf auswählen",
    "language": "Andere Sprache testen",
    "simulation": "Demo mit Skript · Kein echter Anruf",
    "start": "Bageo nimmt ab",
    "next": "Nächster Satz",
    "replay": "Noch einmal",
    "reset": "Zurücksetzen",
    "incoming": "Eingehender Anruf",
    "outgoing": "Bereit zum Anrufen",
    "active": "Bageo spricht",
    "done": "Das Wichtigste steht hier",
    "caller": "Gegenüber",
    "assistant": "Bageo",
    "summary": "Das Wichtigste",
    "progress": "Gesprächsfortschritt",
    "setup": "Loslegen",
    "source": "Auf GitHub ansehen",
    "requirements": "Android mit Root + SIM + Computer + Gemini-Schlüssel",
    "notice": "Vorbereitete Dialoge. Keine Anrufe, Mikrofonzugriffe oder KI-Anfragen. Echte Antworten können abweichen.",
    "auto": "Automatisch abspielen",
    "pause": "Pause",
    "call": "Bageo ruft an",
    "scenes": [
      {
        "id": "delivery",
        "title": "Paketlieferung",
        "teaser": "Ein Paket kommt. Du bist beschäftigt.",
        "description": "Bageo fragt nach den Details. Du entscheidest, was als Nächstes passiert.",
        "outcome": "Persönliche Unterschrift nötig. Erneute Lieferung heute vor 17 Uhr möglich; Zeitpunkt noch bestätigen.",
        "task": "",
        "lines": [
          [
            "caller",
            "Ihr Paket ist da."
          ],
          [
            "assistant",
            "Hallo, ich bin ein KI-Assistent und nehme diesen Anruf entgegen. Muss die empfangende Person selbst unterschreiben?"
          ],
          [
            "caller",
            "Ja. Wir können heute vor siebzehn Uhr noch einmal liefern."
          ],
          [
            "assistant",
            "Ich gebe das weiter, damit die Person einen Zeitpunkt mit Ihnen bestätigt."
          ]
        ]
      },
      {
        "id": "sales",
        "title": "Werbeanruf",
        "teaser": "Ein „Sonderangebot“ ruft an.",
        "description": "Bageo stellt eine Frage nach der anderen. Persönliche Daten bleiben in diesem Beispiel privat.",
        "outcome": "Weiter nach Details gefragt. In diesem Beispiel wurden keine persönlichen Daten weitergegeben.",
        "task": "",
        "lines": [
          [
            "caller",
            "Heute erhalten Sie einen Kredit mit niedrigen Zinsen!"
          ],
          [
            "assistant",
            "Ist das der monatliche oder der jährliche Zinssatz?"
          ],
          [
            "caller",
            "Geben Sie mir zuerst Ihre persönlichen Daten."
          ],
          [
            "assistant",
            "Wie berechnen Sie die Gebühren? Wie hoch ist die monatliche Rate?"
          ]
        ]
      },
      {
        "id": "booking",
        "title": "Tisch buchen",
        "teaser": "Ein Auftrag. Ein Anruf weniger.",
        "description": "Nenne die Aufgabe und deine Grenzen. Bageo ruft für dich an.",
        "outcome": "Beispielergebnis: morgen um 19:30 Uhr, vier Personen. Entspricht deinem Auftrag.",
        "task": "Reserviere morgen für vier um 19 Uhr. 19:30 Uhr geht auch.",
        "lines": [
          [
            "assistant",
            "Hallo, ich bin ein KI-Assistent und möchte morgen um neunzehn Uhr für vier reservieren."
          ],
          [
            "caller",
            "Neunzehn Uhr ist voll. Um neunzehn Uhr dreißig haben wir noch Platz."
          ],
          [
            "assistant",
            "Neunzehn Uhr dreißig passt. Bitte für vier Personen, danke."
          ],
          [
            "caller",
            "Bestätigt: morgen um neunzehn Uhr dreißig, vier Personen."
          ]
        ]
      }
    ]
  },
  "pt": {
    "name": "Português",
    "greeting": "Alô?",
    "tagline": "Seu número. Menos interrupções.",
    "hero": [
      "O telefone tocou?",
      "O Bageo atende."
    ],
    "intro": "Uma IA que atende e faz ligações com seu número de sempre. Em nove idiomas.",
    "choose": "Escolha uma ligação",
    "language": "Experimente outro idioma",
    "simulation": "Demonstração com roteiro · Não é uma ligação real",
    "start": "Deixar o Bageo atender",
    "next": "Próxima fala",
    "replay": "Ver de novo",
    "reset": "Recomeçar",
    "incoming": "Ligação recebida",
    "outgoing": "Pronto para ligar",
    "active": "Bageo está falando",
    "done": "O essencial está anotado",
    "caller": "Interlocutor",
    "assistant": "Bageo",
    "summary": "O que importa",
    "progress": "Progresso da conversa",
    "setup": "Começar",
    "source": "Ver no GitHub",
    "requirements": "Android com root + SIM + computador + chave Gemini",
    "notice": "Exemplos escritos. Sem ligações, microfone ou pedidos à IA. As respostas reais podem variar.",
    "auto": "Reprodução automática",
    "pause": "Pausar",
    "call": "Deixar o Bageo ligar",
    "scenes": [
      {
        "id": "delivery",
        "title": "Entrega",
        "teaser": "A encomenda chegou. Você está ocupado.",
        "description": "O Bageo confirma os detalhes. Você decide o próximo passo.",
        "outcome": "É necessária assinatura em pessoa. Nova entrega possível antes das 17h; falta confirmar o horário.",
        "task": "",
        "lines": [
          [
            "caller",
            "Sua encomenda chegou."
          ],
          [
            "assistant",
            "Olá, sou um assistente de IA que atende este telefone. É necessária a assinatura do destinatário em pessoa?"
          ],
          [
            "caller",
            "Sim. Podemos entregar novamente hoje antes das cinco da tarde."
          ],
          [
            "assistant",
            "Vou avisar para que o destinatário confirme o horário com você."
          ]
        ]
      },
      {
        "id": "sales",
        "title": "Oferta por telefone",
        "teaser": "Uma “oferta especial” está ligando.",
        "description": "O Bageo faz uma pergunta após a outra. Este exemplo não compartilha dados pessoais.",
        "outcome": "Continuou pedindo detalhes. Nenhum dado pessoal foi compartilhado neste exemplo.",
        "task": "",
        "lines": [
          [
            "caller",
            "Você tem um empréstimo com juros baixos aprovado para hoje!"
          ],
          [
            "assistant",
            "Essa taxa de juros é mensal ou anual?"
          ],
          [
            "caller",
            "Primeiro, informe seus dados pessoais."
          ],
          [
            "assistant",
            "Como são calculadas as taxas? Qual é o valor da parcela mensal?"
          ]
        ]
      },
      {
        "id": "booking",
        "title": "Reservar mesa",
        "teaser": "Uma instrução. Uma ligação a menos.",
        "description": "Diga a tarefa e seus limites. O Bageo liga por você.",
        "outcome": "Resultado do exemplo: amanhã às 19h30, quatro pessoas. Conforme suas instruções.",
        "task": "Reserve para quatro amanhã às sete. Sete e meia também pode ser.",
        "lines": [
          [
            "assistant",
            "Olá, sou um assistente de IA. Gostaria de reservar para quatro amanhã às sete."
          ],
          [
            "caller",
            "Às sete está cheio. Temos mesa às sete e meia."
          ],
          [
            "assistant",
            "Sete e meia está ótimo. Para quatro, por favor."
          ],
          [
            "caller",
            "Confirmado: amanhã às sete e meia, quatro pessoas."
          ]
        ]
      }
    ]
  }
};
