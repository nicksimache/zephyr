import bengal from '../bengal-cat-92032-16px.png'
import gray from '../cat-205155-16px.png'
import basket from '../cat-205161-16px.png'
import pink from '../cat-205176-16px.png'
import curled from '../cat-205177-16px.png'
import orange from '../cat-205179-16px.png'
import brown from '../cat-205215-16px.png'
import yarn from '../cat-205221-16px.png'
import black from '../cat-205239-16px.png'
import cream from '../cat-205254-16px.png'
import munchkin from '../munchkin-cat-811001-16px.png'
import persian from '../persian-cat-898481-16px.png'
import ragdoll from '../ragdoll-cat-984469-16px.png'
import siamese from '../siamese-cat-1104285-16px.png'
import tabby from '../tabby-cat-1245576-16px.png'
import redTabby from '../tabby-cat-1245581-16px.png'
import type { Agent } from './api'

export const agentAvatars = [
  { id: 'bengal', label: 'Bengal', image: bengal },
  { id: 'gray', label: 'Gray kitten', image: gray },
  { id: 'basket', label: 'Cat in a basket', image: basket },
  { id: 'pink', label: 'Pink kitten', image: pink },
  { id: 'curled', label: 'Curled orange cat', image: curled },
  { id: 'orange', label: 'Orange kitten', image: orange },
  { id: 'brown', label: 'Brown cat', image: brown },
  { id: 'yarn', label: 'Cat with yarn', image: yarn },
  { id: 'black', label: 'Black cat', image: black },
  { id: 'cream', label: 'Cream kitten', image: cream },
  { id: 'munchkin', label: 'Munchkin', image: munchkin },
  { id: 'persian', label: 'Persian', image: persian },
  { id: 'ragdoll', label: 'Ragdoll', image: ragdoll },
  { id: 'siamese', label: 'Siamese', image: siamese },
  { id: 'tabby', label: 'Tabby', image: tabby },
  { id: 'red-tabby', label: 'Tabby with a red collar', image: redTabby },
]

type Avatar = (typeof agentAvatars)[number]

export function avatarForAgent(agent: Agent): Avatar {
  const seed = Number.parseInt(agent.id.slice(-8), 16) || 0
  return agentAvatars.find(avatar => avatar.id === agent.avatar) ?? agentAvatars[seed % agentAvatars.length]
}

export function randomAvatar(exclude?: string): Avatar {
  const choices = agentAvatars.filter(avatar => avatar.id !== exclude)
  return choices[Math.floor(Math.random() * choices.length)]
}

export function AvatarImage({ avatar, size }: { avatar: Avatar; size: number }) {
  return <img className="cat-sprite" src={avatar.image} alt="" width={size} height={size} />
}
